//! `Noa +ctl`: the shell-callable orchestration client
//! (`docs/specs/agent-orchestration.md` D3/D4/D6). An orchestrator running
//! in one pane spawns worker panes, sends them input, waits for their agent
//! state, reads their output, and closes them — each as one command that
//! prints one JSON object per line.
//!
//! Argument parsing and the wait condition are pure functions so they are
//! unit-testable without a server; [`run`] is the thin I/O shell around them.

mod board;
mod journal;
mod prompt;
mod ready;
mod result;
mod retry;
mod run_attr;
mod timeline;
mod worktree;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use noa_ipc::{
    Client, ClientError, EventKind, MAX_SEND_TEXT_BYTES, Notification, Panel, ScopeSet,
    SendTextOptions, SpawnConfig, SpawnParams, SplitPlacement, TextSource, WireId,
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
                                it; prints {\"paneId\":...,\"cwd\":...}, the
                                worker's cwd resolved (a relative --cwd is
                                taken from this process's cwd; . and ..
                                removed). With --no-board,
                                or no board, it floats over this pane,
                                collapsed to its title strip. A worker's
                                workers go on its orchestrator's board (one
                                level deep; no --no-board).
      --cwd DIR  --env KEY=VALUE  --label TEXT  --input TEXT
      --no-wait-after  --focus  --no-board
                        (--label may not start with board:; --input is
                        raw keys, refused for claude, codex or agy: use
                        send once the agent is up)
      --max-children N (default and most 20: the board holds 20 workers;
                         with --no-board this pane holds 8 floats;
                         workers held open after exiting count; Noa checks
                         it too, so spawns run at once stay under it)
      --worktree        Run the worker in its own git worktree
                        <repo>-wt/LABEL next to the main worktree, on a
                        new branch orch/LABEL (from HEAD; needs --label,
                        refuses --cwd). Prints
                        \"worktree\" and \"branch\" too.
      --tier light|standard|deep
                        Pick the model tier of a claude, codex or agy
                        COMMAND (refused when it already picks a model
                        or effort). Prints \"tier\" too.
      --ready SECS      After the spawn, wait (at most SECS) for the worker
                        to be ready: a report from its agent other than
                        running, or (while no running report is held) a
                        screen that showed something and stayed the same
                        for 1.5 s.
                        Prints \"ready\":\"report\"|\"quiet\"|\"timeout\"
                        (\"gone\" when the pane closed) and \"screen\" (its
                        last 20 non-blank lines).
      --task TEXT | --task-file F  [--timeout SECS] [--stall SECS] [--tail N]
                        [--settle SECS] [--for LIST]
                        For claude, codex or agy: once ready (--ready 30
                        unless given), type the task as send --enter --wait
                        does and print the wait lines after the spawn line,
                        which has \"sent\":true. Only when ready is report or
                        quiet and the screen ends in an agent's input box;
                        otherwise nothing is typed: the line has
                        \"sent\":false (exit 0), so read \"screen\" (a trust
                        prompt, a login) and send yourself.
                        Every worker gets its own file to write its final
                        report to: NOA_RESULT=<its cwd>/.noa/result-LABEL.md
                        (a plain --label; result-LABEL-<8 hex>.md when that
                        file exists; else result-<8 random hex>.md).
                        .noa/ holds a .gitignore of *, so git never sees it.
                        Prints \"result\" too (none, with a warning, when the
                        cwd is not writable).
  send PANE [--enter] [--raw] [--replace] [--wait [wait options]] [--answer]
       [--queue] [--new-run] [--any-pane] [--] TEXT|-
                                Type TEXT (or stdin) into PANE; prints the
                                report count to pass to `wait --after`. Refused
                                while PANE waits for an approval or for the
                                answer to a question, unless --answer, and while
                                its agent is running, unless --queue (then every
                                --wait line has \"queued\":true: the wait may end
                                with the turn already running); --raw keys (Esc,
                                Ctrl-C) still go to a running agent. Pasted text
                                for an agent (or a pane whose program is not
                                listed yet, unless spawn started it with a
                                command) waits up to 15 s for it to enable
                                bracketed paste, before any key; if it does not,
                                nothing is sent (exit 1).
                                A TEXT over 250 KiB (JSON-encoded) is refused:
                                send a file's path instead. --replace first
                                clears what the input line holds (Ctrl-E, then
                                Ctrl-U per line, up to 32 lines). --wait prints
                                how to resume on stderr once the text is in.
                                --new-run starts a new piece of work: when Noa
                                has a board timeline, PANE gets a new run and
                                its header is typed before TEXT (spawn --task
                                and retry do this themselves).
  wait PANE...|--children [--after SEQ | --after PANE=SEQ ...] [--for LIST]
       [--all] [--timeout SECS] [--tail N] [--stall SECS] [--settle SECS]
       [--any-pane]
                                Block until a worker stops. LIST is a comma list
                                of finished,permission,input,error,exit,attention
                                (default: all of them), or stalled (needs
                                --stall). --stall SECS also ends the wait for a
                                pane whose screen text has not changed for SECS
                                seconds (reason \"stalled\", with \"quietSecs\"),
                                for an agent that does not report. --settle
                                SECS holds a finished for SECS seconds: a newer
                                report meanwhile drops it and the wait goes on,
                                a change of the work above the input box starts
                                the SECS again; one that held is printed with
                                \"settledSecs\".
                                --tail adds each open pane's last N output lines
                                as \"tail\" (for an agent, the lines above its
                                input box). A permission or input line has
                                \"prompt\": the screen's block below the work
                                (the dialog or input box, up to 20 lines, rules
                                removed), with or without --tail. A timeout
                                prints each pending pane's report count as
                                `baseline`; pass it back as --after PANE=SEQ.
                                SIGTERM, SIGINT or SIGHUP prints that line too,
                                with \"interrupted\":true (exit 2). A line gains
                                \"result\":{\"path\",\"bytes\"} once the pane's
                                result file was written since the wait (or the
                                send of send --wait, or the send that --after
                                SEQ resumes, from this pane's journal) began,
                                and \"blocked\" when the agent reported what it
                                is blocked on.
  read PANE [--scrollback] [--max-bytes N]
                                Print PANE's screen (or retained output) text;
                                a cut keeps the end and says so on stderr. A
                                pane on the alternate screen keeps no history:
                                --scrollback (and wait --tail) then says on
                                stderr that only its screen came back.
  result PANE [--max-bytes N | --path]
                                Print PANE's result file: its spawn's
                                NOA_RESULT from this pane's journal (or where
                                close --prune moved it), else
                                <its cwd>/.noa/result-LABEL.md for a pane with a
                                plain label. Without --path, exit 1 when no
                                file is known or the file is missing or empty.
                                --max-bytes keeps the start and says so on
                                stderr. --path prints only the file's path
                                whether it exists or not (e.g. to tail a
                                command's log) and exits 0 when a path is
                                known, and exits 1 when no path is known; it
                                does not go with --max-bytes.
  status [--answers]            One line per worker of this pane: its panel's
                                state with what this pane's journal knows
                                (tier, argv, lastTask, lastSentSeq, lastOutcome,
                                answers, result; ahead and dirty for a worktree
                                worker); workers the journal knows that are gone
                                come last with \"gone\":true. --answers prints
                                every prompt answered from here, oldest first.
  close PANE...|--children [--any-pane] [--prune]
                                Close worker panes. One that is already gone
                                prints \"gone\":true and is not an error.
                                --prune also removes a closed worker's
                                `spawn --worktree` worktree and its branch
                                (git worktree remove, git branch -d; never
                                forced, so unsaved work stays and is named
                                under \"worktree\":{\"kept\":...}), found
                                from the pane's cwd (for one already gone,
                                from this pane's journal); a labelled pane
                                with no such worktree also gets \"kept\"
                                saying why.
                                Its .noa/result-*.md files first move to the
                                main worktree's .noa/ (a taken name gets
                                -<8 hex>); \"worktree\":{\"result\"} is where
                                the worker's went, \"moved\" lists every move
                                (from, to, and the paneId of another worker
                                whose file it was), and result and status read
                                each there. One that cannot move keeps the
                                worktree, with \"kept\" saying why.
  retry PANE [--tier light|standard|deep] [--task TEXT | --task-file F]
       [--ready SECS] [--timeout SECS] [--stall SECS] [--tail N]
       [--settle SECS] [--for LIST] [--all] [--no-board]
                                Start this pane's claude, codex or agy worker
                                PANE (listed or gone, by this pane's journal)
                                again: its command with the old tier's flags
                                swapped for --tier's (else the old tier's), in
                                its directory (its worktree and branch, none
                                made) under its label; PANE, if listed, is
                                closed first (never pruned). Refused while
                                PANE's agent is running. Types its last task
                                (or --task/--task-file) after a preface: the
                                previous worker's changes are here, check git
                                status/log and build on them; its report path.
                                Then as spawn --task: prints the spawn line
                                with \"retryOf\" (and \"tier\"), then the
                                wait lines. A task the journal kept only the
                                start of needs --task-file.
  report STATE [[--] DETAIL]    Write an agent-state report (running, permission,
                                input, finished, error, clear) to this terminal.
  board [--of PANE|self] [--timeline [--board-id B]]
                                Show an orchestrator's workers in this pane
                                (default: this pane's own orchestrator, else
                                every worker; self: this pane's own, for a
                                board that is its own orchestrator). Keys are
                                listed at the bottom. --timeline starts in its
                                timeline (t switches), or board B's.
  board --open [--split right|down | --tab]
                                Open a board on this pane's workers next to it
                                (an open one is printed with \"existing\":true).
  timeline context [--pane PANE] | boards [--limit N]
         | post [--board B] [--run R] [--request-id ID]
                (--file F | --from-result PATH --title T --summary S
                 --outcome O [--kind K] [--take-over])
         | list [--board B] [--before SEQ] [--limit N] [filters] [--query Q]
         | show [--board B] POST | changes [--board B] [--after SEQ]
         | export [--board B] --format markdown|json --output PATH
                                Read or write a board's timeline (default
                                board: this pane's). One JSON document on
                                stdout; exit 3 when Noa has no timeline.

Use `--` before a TEXT or DETAIL that starts with `--`. A pane that exits
and stays open ends a wait whatever --for lists, as a closed pane does; one
that had exited before the wait began only ends it when no other pane is left.
`send`, `close` and `wait PANE` only accept this pane's own workers unless
--any-pane is given. Exit status: 0 ok, 1 error, 2 wait timed out, 3 server
unavailable, or a request Noa did not confirm in time (it may still have
taken effect: check with `ls` or `read` before retrying).
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
    /// `retry PANE`: close a worker and start it again with its task.
    Retry(RetryArgs),
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
        of: Option<BoardOf>,
        /// `--open`: start a board pane next to this one instead.
        open: Option<Placement>,
    },
    /// `board --timeline [--board-id B] [--of PANE|self]`: run the board
    /// here, starting in its timeline view (`docs/specs/board-timeline.md`
    /// AC-3).
    BoardTimeline {
        of: Option<BoardOf>,
        board_id: Option<String>,
    },
    /// `status [--answers]`: this pane's workers with what the journal
    /// knows of them, or every answered prompt.
    Status {
        answers: bool,
    },
    /// `result PANE [--max-bytes N | --path]`: print a worker's result file,
    /// or only its path.
    Result {
        pane: u64,
        max_bytes: Option<usize>,
        path: bool,
    },
    /// `timeline SUBCOMMAND ...`: a board's timeline
    /// (`docs/specs/board-timeline.md` AC-2).
    Timeline(timeline::TimelineCommand),
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
    /// `--ready SECS`: wait this long at most for the worker to be ready.
    pub(crate) ready: Option<Duration>,
    /// `--task`/`--task-file`: type a task once the worker is ready.
    pub(crate) task: Option<Task>,
    pub(crate) command: Vec<String>,
}

/// `retry PANE [--tier T] [--task TEXT | --task-file F] [--ready SECS]
/// [wait options] [--no-board]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RetryArgs {
    pub(crate) pane: u64,
    /// `--tier`: the new worker's tier (default: the old one's).
    pub(crate) tier: Option<worktree::Tier>,
    /// `--task`/`--task-file` (default: the old worker's last task).
    pub(crate) task: Option<TaskText>,
    pub(crate) ready: Option<Duration>,
    /// The options of the wait after the task, as `spawn --task` takes.
    pub(crate) wait: WaitOptions,
    pub(crate) board: bool,
}

/// `spawn --task TEXT` or `--task-file F`, with the options of the wait
/// that follows it (as `send --enter --wait`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Task {
    pub(crate) text: TaskText,
    pub(crate) wait: WaitOptions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TaskText {
    Literal(String),
    File(String),
}

impl TaskText {
    fn read(&self) -> Result<String, Failure> {
        match self {
            Self::Literal(text) => Ok(text.clone()),
            Self::File(file) => std::fs::read_to_string(file)
                .map_err(|err| Failure::error(format!("cannot read --task-file {file:?}: {err}"))),
        }
    }
}

impl Task {
    fn prepare(self, supports_timeline: bool) -> Result<(String, WaitOptions), Failure> {
        let text = self.text.read()?;
        check_send_size(&run_attr::prospective_text(&text, supports_timeline))
            .map_err(Failure::error)?;
        Ok((text, self.wait))
    }
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
    /// `--queue`: type into a pane whose agent is still working.
    pub(crate) queue: bool,
    /// `--new-run`: register a new timeline run for PANE and type its
    /// header before the text.
    pub(crate) new_run: bool,
    /// `--wait`: block like `wait PANE --after <seq before sending>`.
    pub(crate) wait: Option<WaitOptions>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WaitArgs {
    pub(crate) panes: PaneSet,
    /// `--any-pane`: wait on a named pane that is not this pane's worker.
    pub(crate) any_pane: bool,
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
    /// Set by `send --queue`: the text went to a working agent, so the turn
    /// that ends the wait may be the one already running. Every line says so.
    pub(crate) queued: bool,
    /// `--settle SECS`: hold a `finished` this long, and keep waiting when a
    /// newer report or a change of the work on screen comes meanwhile.
    pub(crate) settle: Option<Duration>,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            reasons: Reason::ALL.to_vec(),
            all: false,
            timeout: None,
            tail: None,
            stall: None,
            queued: false,
            settle: None,
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
    if name == "timeline" {
        return timeline::parse(rest).map(Command::Timeline);
    }
    let mut rest = Args::new(rest);
    let command = match name.as_str() {
        "self" => Command::SelfPanel,
        "ls" => Command::Ls {
            children: rest.flag("--children"),
        },
        "spawn" => Command::Spawn(parse_spawn(&mut rest)?),
        "retry" => Command::Retry(parse_retry(&mut rest)?),
        "send" => Command::Send(parse_send(&mut rest)?),
        "wait" => {
            let mut afters = Vec::new();
            while let Some(value) = rest.value("--after")? {
                afters.push(value);
            }
            let any_pane = rest.flag("--any-pane");
            let options = parse_wait_options(&mut rest)?;
            let panes = pane_set(&mut rest)?;
            let after = parse_afters(&afters, &panes)?;
            Command::Wait(WaitArgs {
                panes,
                any_pane,
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
            let timeline = rest.flag("--timeline");
            let board_id = rest.value("--board-id")?;
            let placement = parse_placement(&mut rest)?;
            let of = rest
                .value("--of")?
                .map(|v| match v.as_str() {
                    "self" => Ok(BoardOf::Own),
                    _ => parse_u64("--of", &v).map(BoardOf::Pane),
                })
                .transpose()?;
            if !opening && placement != Placement::Auto {
                return Err("--split and --tab go with --open".to_string());
            }
            if opening && of == Some(BoardOf::Own) {
                return Err("--of self runs the board here; it does not go with --open".to_string());
            }
            if opening && timeline {
                return Err(
                    "--timeline runs the board here; it does not go with --open".to_string()
                );
            }
            if let Some(board) = &board_id {
                if !timeline {
                    return Err("--board-id goes with --timeline".to_string());
                }
                if !noa_ipc::timeline::valid_id(noa_ipc::timeline::IdKind::Board, board) {
                    return Err(format!(
                        "--board-id {board:?} is not bd-<16 lowercase hex digits>"
                    ));
                }
            }
            if timeline {
                Command::BoardTimeline { of, board_id }
            } else {
                Command::Board {
                    of,
                    open: opening.then_some(placement),
                }
            }
        }
        "status" => Command::Status {
            answers: rest.flag("--answers"),
        },
        "result" => {
            let max_bytes = rest.value("--max-bytes")?;
            let path = rest.flag("--path");
            if path && max_bytes.is_some() {
                return Err("--path and --max-bytes conflict: --path reads no file".to_string());
            }
            let max_bytes = max_bytes
                .map(|v| parse_u64("--max-bytes", &v).map(|n| n as usize))
                .transpose()?;
            Command::Result {
                pane: single_pane(&mut rest)?,
                max_bytes,
                path,
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
    // A board is known by its label alone (`board_pane`).
    if label
        .as_deref()
        .is_some_and(|label| label.starts_with(BOARD_LABEL_PREFIX))
    {
        return Err(format!(
            "--label may not start with {BOARD_LABEL_PREFIX:?}: that names an orchestrator's board"
        ));
    }
    let input = rest.value("--input")?;
    if input.is_some() && worktree::is_agent_command(&command) {
        return Err(
            "--input is written as raw keys right after the worker starts, so an agent takes \
             each newline as Enter; start the agent, then give it the task with \
             `send PANE --enter`"
                .to_string(),
        );
    }
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
    let ready = parse_ready(rest)?;
    let task = match parse_task_text(rest)? {
        Some(text) => {
            if !worktree::is_agent_command(&command) {
                return Err(
                    "--task types the task into claude, codex or agy once it is ready: give one \
                     of them as COMMAND after `--`"
                        .to_string(),
                );
            }
            Some(Task {
                text,
                wait: parse_wait_options(rest)?,
            })
        }
        None => None,
    };
    Ok(SpawnArgs {
        board,
        cwd,
        env,
        label,
        input,
        wait_after: !rest.flag("--no-wait-after"),
        focus: rest.flag("--focus"),
        max_children,
        worktree,
        tier,
        ready,
        task,
        command,
    })
}

/// `--ready SECS`.
fn parse_ready(rest: &mut Args) -> Result<Option<Duration>, String> {
    rest.value("--ready")?
        .map(|value| {
            value
                .parse::<f64>()
                .ok()
                .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
                .ok_or_else(|| format!("--ready expects seconds, got {value:?}"))
        })
        .transpose()
}

/// `--task TEXT` or `--task-file F`.
fn parse_task_text(rest: &mut Args) -> Result<Option<TaskText>, String> {
    Ok(match (rest.value("--task")?, rest.value("--task-file")?) {
        (Some(_), Some(_)) => return Err("--task and --task-file are exclusive".to_string()),
        (Some(text), None) => Some(TaskText::Literal(text)),
        (None, Some(file)) => Some(TaskText::File(file)),
        (None, None) => None,
    })
}

fn parse_retry(rest: &mut Args) -> Result<RetryArgs, String> {
    let board = !rest.flag("--no-board");
    let tier = rest
        .value("--tier")?
        .map(|name| worktree::Tier::parse(&name))
        .transpose()?;
    let ready = parse_ready(rest)?;
    let task = parse_task_text(rest)?;
    let wait = parse_wait_options(rest)?;
    Ok(RetryArgs {
        pane: single_pane(rest)?,
        tier,
        task,
        ready,
        wait,
        board,
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
    let queue = rest.flag("--queue");
    let new_run = rest.flag("--new-run");
    if new_run && (answer || raw) {
        // An answer or raw keys are not a new piece of work, and a header
        // typed into a prompt would answer it.
        return Err("--new-run does not go with --answer or --raw".to_string());
    }
    if new_run && (queue || replace) {
        // A queued follow-up or a rewritten draft is the same piece of work
        // (board-timeline AC-4): neither registers a run.
        return Err(
            "--new-run does not go with --queue or --replace: they continue the same run"
                .to_string(),
        );
    }
    if replace && answer {
        // The keys that clear an input line would land in the prompt.
        return Err("--replace does not go with --answer: a prompt has no input line".to_string());
    }
    let wait = if rest.flag("--wait") {
        Some(WaitOptions {
            queued: queue,
            ..parse_wait_options(rest)?
        })
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
        queue,
        new_run,
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
    if let Some(value) = rest.value("--settle")? {
        options.settle = Some(positive_secs("--settle", &value)?);
    }
    if let Some(value) = rest.value("--stall")? {
        options.stall = Some(positive_secs("--stall", &value)?);
        if !options.reasons.contains(&Reason::Stalled) {
            options.reasons.push(Reason::Stalled);
        }
    } else if options.reasons.contains(&Reason::Stalled) {
        return Err("--for stalled needs --stall SECS".to_string());
    }
    Ok(options)
}

/// A `--stall`/`--settle` value: seconds above 0.
fn positive_secs(name: &str, value: &str) -> Result<Duration, String> {
    let seconds: f64 = value
        .parse()
        .ok()
        .filter(|seconds: &f64| seconds.is_finite() && *seconds > 0.0)
        .ok_or_else(|| format!("{name} expects seconds above 0, got {value:?}"))?;
    Duration::try_from_secs_f64(seconds).map_err(|_| format!("{name} is out of range: {value:?}"))
}

/// Seconds as a JSON number: whole ones as an integer.
fn secs_json(duration: Duration) -> serde_json::Value {
    if duration.subsec_nanos() == 0 {
        duration.as_secs().into()
    } else {
        duration.as_secs_f64().into()
    }
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
    /// The pane's label when the wait began, for a line printed after the
    /// pane is gone.
    pub(crate) label: Option<String>,
    /// An agent ran in the pane when the wait began (`agent.kind`).
    pub(crate) had_agent: bool,
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
            label: panel.and_then(|panel| panel.label.clone()),
            had_agent: panel
                .and_then(|panel| panel.agent.as_ref())
                .is_some_and(|agent| agent.kind.is_some()),
        }
    }
}

/// What ended a wait for one pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) reason: Reason,
    /// The pane is gone (closed, or its child exited without being held).
    pub(crate) closed: bool,
    /// The agent that ran in the pane quit back to its shell (`exit`).
    pub(crate) agent_left: bool,
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
            agent_left: false,
        });
    };
    // A held-open exited pane will never report again either, so it ends
    // the wait like a vanished pane does, whatever `--for` asked for.
    if panel.exit.is_some() {
        return Some(Outcome {
            reason: Reason::Exit,
            closed: false,
            agent_left: false,
        });
    }
    // An agent that quit back to its shell reports nothing more, and its
    // card's state is cleared without a new report: nothing else would end
    // the wait.
    if watch.had_agent
        && panel
            .agent
            .as_ref()
            .is_none_or(|agent| agent.kind.is_none() && agent.state.is_none())
    {
        return Some(Outcome {
            reason: Reason::Exit,
            closed: false,
            agent_left: true,
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
            agent_left: false,
        });
    }
    if !panel.attention {
        watch.attention_armed = true;
    } else if watch.attention_armed && reasons.contains(&Reason::Attention) {
        return Some(Outcome {
            reason: Reason::Attention,
            closed: false,
            agent_left: false,
        });
    }
    None
}

fn outcome_json(
    watch: &Watch,
    outcome: &Outcome,
    panel: Option<&Panel>,
    quiet: Option<Duration>,
) -> serde_json::Value {
    let agent = panel.and_then(|panel| panel.agent.as_ref());
    let mut value = serde_json::json!({
        "paneId": WireId(watch.pane),
        "reason": outcome.reason.name(),
    });
    let object = value.as_object_mut().expect("object literal");
    if let Some(label) = panel
        .and_then(|panel| panel.label.as_ref())
        .or(watch.label.as_ref())
    {
        object.insert("label".into(), label.clone().into());
    }
    if outcome.closed {
        object.insert("closed".into(), true.into());
    }
    if outcome.agent_left {
        object.insert("agentLeft".into(), true.into());
    }
    if let Some(agent) = agent {
        object.insert("seq".into(), agent.seq.into());
        if let Some(state) = &agent.state {
            object.insert("state".into(), state.clone().into());
        }
        if let Some(detail) = &agent.detail {
            object.insert("detail".into(), detail.clone().into());
        }
        if let Some(blocked) = &agent.blocked {
            object.insert("blocked".into(), blocked.clone().into());
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
    /// A request ran past the client's deadline on a connection that is
    /// still open; a long `wait` retries it instead of giving up.
    transient: bool,
}

impl Failure {
    fn error(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_ERROR,
            message: message.into(),
            transient: false,
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_UNAVAILABLE,
            message: message.into(),
            transient: false,
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
/// The server's JSON-RPC code for a request the app did not answer in time:
/// it may still have been carried out.
const RPC_OUTCOME_UNKNOWN: i64 = -32009;
/// The server's JSON-RPC code for a paste refused with `requirePasteMode`:
/// the pane has not enabled bracketed paste yet.
const RPC_NOT_READY: i64 = -32010;
/// The server's JSON-RPC code for a spawn refused at its `maxChildren`.
const RPC_LIMIT_REACHED: i64 = -32011;

const SCOPES_HINT: &str = "enable it in Noa's config with `server-enable = true` and `server-scopes = read,control,input`";

/// Said when the server closed a connection at its connection limit (1013).
const TOO_MANY_CONNECTIONS: &str = "Noa has too many control connections open (each running \
     `wait`, `send --wait`, board and attach holds one); end some, or try again later";

/// The pauses between connects that the server refused at its connection
/// limit: 100 ms, doubling, 3 s in all.
pub(crate) fn connect_retry_pauses() -> Vec<Duration> {
    const TOTAL: Duration = Duration::from_secs(3);
    let mut pauses = Vec::new();
    let (mut next, mut spent) = (Duration::from_millis(100), Duration::ZERO);
    while spent < TOTAL {
        let pause = next.min(TOTAL - spent);
        pauses.push(pause);
        spent += pause;
        next *= 2;
    }
    pauses
}

/// `connect`, tried again after each of [`connect_retry_pauses`] while the
/// server refuses it at its connection limit: a slot frees up as soon as
/// another command ends. Past the few refusals it closes with 1013, a full
/// server drops a connection during the handshake, so that is retried in
/// the same 3 s too.
pub(crate) fn connect_retrying<T>(
    mut connect: impl FnMut() -> Result<T, ClientError>,
    mut pause: impl FnMut(Duration),
) -> Result<T, ClientError> {
    let mut pauses = connect_retry_pauses().into_iter();
    loop {
        match connect() {
            Err(err @ (ClientError::TooManyConnections(_) | ClientError::HandshakeDropped(_))) => {
                match pauses.next() {
                    Some(duration) => pause(duration),
                    None => return Err(err),
                }
            }
            result => return result,
        }
    }
}

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
            ClientError::Rpc {
                code: RPC_LIMIT_REACHED,
                message,
            } => Failure::error(message),
            // The server went away mid-command (stopped or restarted).
            ClientError::Closed(_)
            | ClientError::Transport(_)
            | ClientError::HandshakeDropped(_) => {
                Failure::unavailable(format!("lost the connection to Noa: {error}"))
            }
            ClientError::TooManyConnections(_) => {
                Failure::unavailable(format!("{TOO_MANY_CONNECTIONS} ({error})"))
            }
            // A request over the server's size bound: not a Noa gone away.
            ClientError::MessageTooBig(_) => Failure::error(format!(
                "Noa closed the connection because a request was over its size limit ({error}); \
                 for a long text, write it to a file and send its path"
            )),
            // Outside a wait, a request that timed out ends the command as
            // before; only the wait loop retries it.
            // The app took too long, so the server gave up waiting: the
            // request may still have been carried out (D16).
            ClientError::Rpc {
                code: RPC_OUTCOME_UNKNOWN,
                message,
            } => Failure::unavailable(format!(
                "Noa did not confirm the request in time ({message}); it may still have taken \
                 effect: check with `ls` or `read` before retrying"
            )),
            ClientError::Timeout(_) => Failure {
                transient: true,
                ..Failure::unavailable(format!("Noa did not answer in time: {error}"))
            },
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
    /// This pane's journal of what its commands did (AU-04).
    journal: journal::Journal,
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
    } else if matches!(error, ClientError::TooManyConnections(_)) {
        format!("; {TOO_MANY_CONNECTIONS} (tried again for 3 s)")
    } else if matches!(error, ClientError::HandshakeDropped(_)) {
        format!(
            "; Noa may be at its connection limit: {TOO_MANY_CONNECTIONS} (tried again for 3 s)"
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
    let cwd = spawn_cwd(spawn.cwd.clone(), current.clone());
    if let Some(given) = &spawn.cwd {
        // The path as given, not the normalized one: dropping a `..` can
        // turn a path the kernel refuses (`file/../dir`, `locked/.`) into
        // a directory.
        let joined = joined_cwd(Some(given.clone()), current);
        let joined = joined.as_deref().unwrap_or(std::path::Path::new(given));
        if !joined.is_dir() {
            let resolved = if joined == std::path::Path::new(given) {
                String::new()
            } else {
                format!(" (resolved to {joined:?})")
            };
            return Err(format!("--cwd {given:?}{resolved} is not a directory"));
        }
    }
    if let Some(program) = spawn.command.first()
        && std::path::Path::new(program).is_absolute()
    {
        // `retry` closes the old worker after this check, so a program
        // removed since it was started must be refused here. A relative
        // path stays the worker's to resolve.
        if !is_executable(std::path::Path::new(program)) {
            return Err(format!(
                "command not found: {program:?} is not an executable file"
            ));
        }
    } else if let (Some(program), Some(path)) = (spawn.command.first(), path)
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

/// `board --of`: whose workers the board shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoardOf {
    Pane(u64),
    /// `--of self`: the board's own pane's (agent-orchestration D20).
    Own,
}

/// The label of `own`'s board pane, which a later spawn looks for.
pub(crate) fn board_label(own: u64) -> String {
    format!("{BOARD_LABEL_PREFIX}{own}")
}

/// What every board pane's label starts with; a worker's may not.
const BOARD_LABEL_PREFIX: &str = "board:";

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

/// The failure of a command that needs this process's own pane when the
/// server does not list it: a server that names no surface ids is probably
/// another Noa (exit 3), otherwise the pane is just not there (exit 1).
pub(crate) fn own_missing_failure(message: String, lacks_ids: bool) -> Failure {
    if lacks_ids {
        Failure::unavailable(message)
    } else {
        Failure::error(message)
    }
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

/// How [`connect_client`] treats a server at its connection limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectRetry {
    /// Tried again for 3 s ([`connect_retrying`]): a command.
    ForThreeSeconds,
    /// Tried once: the board backs off between attempts itself and must
    /// not sleep through them on its UI thread.
    Once,
}

/// `connect` under `retry`.
pub(crate) fn connect_with<T>(
    retry: ConnectRetry,
    mut connect: impl FnMut() -> Result<T, ClientError>,
    pause: impl FnMut(Duration),
) -> Result<T, ClientError> {
    match retry {
        ConnectRetry::ForThreeSeconds => connect_retrying(connect, pause),
        ConnectRetry::Once => connect(),
    }
}

/// A control connection to the Noa this process's config names, with its
/// token, and the port it is on.
pub(crate) fn connect_client(retry: ConnectRetry) -> Result<(Client, u16), Failure> {
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
    connect_with(
        retry,
        || {
            Client::connect(
                &url,
                &token,
                ScopeSet::from_strings(["read", "control", "input"]),
            )
        },
        std::thread::sleep,
    )
    .map(|client| (client, config.server_port))
    .map_err(|err| match err {
        ClientError::Rpc { .. } => Failure::unavailable(err.to_string()),
        other => Failure::unavailable(connect_failure_message(&url, &other)),
    })
}

impl Session {
    fn connect() -> Result<Self, Failure> {
        let (client, port) = connect_client(ConnectRetry::ForThreeSeconds)?;
        let mut session = Self {
            client,
            own: None,
            port,
            waited_for_own: false,
            lacks_surface_ids: false,
            journal: journal::Journal::open(),
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
            own_missing_failure(message, self.lacks_surface_ids)
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
        guard_targets(&panels, panes, own).map_err(Failure::error)
    }

    /// Refuse a wait on a listed pane that is neither this pane's worker
    /// nor this pane, as `send` and `close` refuse one: after Noa restarts,
    /// pane ids are given out again, and a remembered id can name another
    /// pane. Outside a listed pane there are no workers to check against,
    /// and the wait runs as before.
    ///
    /// Its listing is taken as the wait's first ([`list_before_wait`]):
    /// `false` when a caught signal ended the wait there.
    fn guard_wait(
        &mut self,
        panes: &[u64],
        known: &WaitStart,
        deadline: Option<Instant>,
    ) -> Result<bool, Failure> {
        let Some(own) = self.own_now()? else {
            return Ok(true);
        };
        let Some(panels) =
            list_before_wait(&mut self.client, known, panes, deadline, &mut |line| {
                print_json(line)
            })?
        else {
            return Ok(false);
        };
        wait_guard(&panels, panes, own).map_err(Failure::error)?;
        Ok(true)
    }

    /// This pane, for a command that only targets its own workers.
    fn guard_own(&mut self) -> Result<u64, Failure> {
        self.own_now()?.ok_or_else(|| {
            let message = format!(
                "{} Without a pane of its own there are no workers to target; pass \
                 --any-pane to target a pane anyway.",
                not_own_message(
                    std::env::var(SURFACE_ID_ENV).ok().as_deref(),
                    self.port,
                    self.lacks_surface_ids
                )
            );
            own_missing_failure(message, self.lacks_surface_ids)
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
        // The worktree is found from the pane's cwd, so this command's own
        // cwd (another worktree, or outside the repository) does not matter.
        // The pane's result file is moved out of it first ([`result_path`]).
        let workers = journal::workers(&self.journal.events());
        let mut prune_label = |pane: u64, label: &str, pane_cwd: &str| {
            if !prune {
                return None;
            }
            let pane_cwd = Path::new(pane_cwd);
            let result = workers
                .iter()
                .find(|worker| worker.pane == pane)
                .and_then(|worker| worker.result.as_ref())
                .map(PathBuf::from)
                .or_else(|| result::labelled_path(pane_cwd, Some(label)));
            // Other workers can have worked in this worktree too (`--cwd`).
            let others: Vec<(u64, PathBuf)> = workers
                .iter()
                .filter(|worker| worker.pane != pane)
                .filter_map(|worker| Some((worker.pane, PathBuf::from(worker.result.as_ref()?))))
                .collect();
            worktree::prune(label, pane_cwd, result.as_deref(), &others)
        };
        let journal = &mut self.journal;
        close_panes(
            &mut self.client,
            panes,
            own,
            &workers,
            &mut prune_label,
            &mut |line| {
                if let Some(pane) = line_pane(line) {
                    journal.append(
                        "close",
                        serde_json::json!({ "pane": pane, "worktree": line.get("worktree") }),
                    );
                }
                print_json(line);
            },
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
            Command::Spawn(args) => {
                return self.spawn(args, serde_json::Map::new(), run_attr::NewRun::SpawnTask);
            }
            Command::Retry(args) => return self.retry(args),
            Command::Send(args) => return self.send(args),
            Command::Wait(args) => {
                let panes = self.resolve(&args.panes)?;
                if panes.is_empty() {
                    return Err(Failure::error("no panes to wait for"));
                }
                let named = matches!(args.panes, PaneSet::Ids(_));
                let guard = named && !args.any_pane;
                return self.wait(&panes, named, guard, &args.after, &args.options);
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
                if let Some(notice) = history_notice(pane, source, &result) {
                    eprintln!("{notice}");
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
            Command::Board { of, open } => return self.board(of, open, board::Open::default()),
            Command::BoardTimeline { of, board_id } => {
                let view = board::Open {
                    timeline: true,
                    board_id,
                };
                return self.board(of, None, view);
            }
            Command::Status { answers } => return self.status(answers),
            Command::Result {
                pane,
                max_bytes,
                path,
            } => return self.result(pane, max_bytes, path),
            Command::Timeline(command) => {
                return timeline::execute(&mut self.client, self.own, command);
            }
            Command::Report { .. } => unreachable!("handled before connecting"),
        }
        Ok(EXIT_OK)
    }

    /// `board`: run the board here, or with `--open` start one next to
    /// this pane. The board pane has no parent, so it never counts as one
    /// of this pane's workers.
    fn board(
        &mut self,
        of: Option<BoardOf>,
        open: Option<Placement>,
        view: board::Open,
    ) -> Result<i32, Failure> {
        let Some(placement) = open else {
            let of = match of {
                Some(BoardOf::Pane(of)) => Some(of),
                // D20: a board tab orchestrates itself; its pane's workers
                // are the ones shown.
                Some(BoardOf::Own) => Some(self.require_own()?),
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
            return board::run(&mut self.client, of, self.own, view);
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
            max_children: None,
            auto_approve: None,
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
                    // Not when the split's outcome is unknown: it may
                    // still open, and a tab would be a second board.
                    Err(ClientError::Rpc { code, .. }) if code != RPC_OUTCOME_UNKNOWN => {
                        self.client.spawn_tab(window, &params)?
                    }
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

    /// `spawn`; `extra` goes on the spawn line and its journal event
    /// (`retry`'s `retryOf`, and the worktree and branch it reuses).
    /// `new_run`: why a task, once it is to be typed, gets a timeline run.
    fn spawn(
        &mut self,
        args: SpawnArgs,
        extra: serde_json::Map<String, serde_json::Value>,
        new_run: run_attr::NewRun,
    ) -> Result<i32, Failure> {
        // Read and checked before anything starts.
        let task = args
            .task
            .map(|task| task.prepare(self.client.supports_board_timeline()))
            .transpose()?;
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
        // Also checked by the server, atomically with the spawn (REL-02);
        // this check is for a server that predates `maxChildren`.
        let (workers, exited) = children_counts(&children, own);
        if workers >= args.max_children {
            return Err(Failure::error(max_children_message(
                workers,
                exited,
                args.max_children,
            )));
        }
        let max_children = args.max_children;
        let mut env = worker_env(args.env, !args.command.is_empty(), std::env::vars());
        let mut cwd = spawn_cwd(args.cwd, std::env::current_dir().ok());
        // `--worktree` refuses `--cwd`; the worktree replaces this one.
        let command = match args.tier {
            Some(tier) => worktree::apply_tier(&args.command, tier).map_err(Failure::error)?,
            None => args.command,
        };
        let worktree_label = args.worktree.then(|| args.label.clone()).flatten();
        let label = args.label;
        let wanted_label = label.clone();
        let earlier: Vec<u64> = children.iter().map(|panel| panel.pane_id.0).collect();
        let (input, wait_after, focus) = (args.input, args.wait_after, args.focus);
        let argv = command.clone();
        // The worker's cwd and result file, once its worktree is known.
        let mut started_in: Option<String> = None;
        let mut result_path: Option<String> = None;
        let spawned = spawn_steps(
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
                    return Host {
                        pane: own,
                        opened_board: false,
                    };
                }
                let panels = session.client.list_panels().map_err(Failure::from);
                let mut opened_board = false;
                let (pane, warning) = board_before_spawn(
                    || panels,
                    own,
                    || {
                        let opened = session.open_board(own, Placement::Auto);
                        opened_board = opened.is_ok();
                        opened
                    },
                );
                if let Some(warning) = warning {
                    eprintln!("Noa +ctl: {warning}");
                }
                Host { pane, opened_board }
            },
            |session, host, made| {
                if let Some((_, made)) = made {
                    cwd = made.path.to_str().map(str::to_string);
                }
                if let Some(dir) = cwd.as_deref()
                    && !env.contains_key(result::RESULT_ENV)
                {
                    match result::prepare(Path::new(dir), label.as_deref()) {
                        Ok(path) => {
                            env.insert(
                                result::RESULT_ENV.to_string(),
                                path.to_string_lossy().into_owned(),
                            );
                        }
                        Err(err) => eprintln!(
                            "Noa +ctl: warning: the worker gets no ${}: cannot write {dir}/.noa: \
                             {err}",
                            result::RESULT_ENV
                        ),
                    }
                }
                result_path = env.get(result::RESULT_ENV).cloned();
                started_in = cwd.clone();
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
                    max_children: u32::try_from(max_children).ok(),
                    auto_approve: None,
                };
                session
                    .client
                    .spawn_split(host, SplitPlacement::Float, &params)
                    .map_err(|err| {
                        limit_reached_said(err, own, max_children, || {
                            session.client.list_panels().ok()
                        })
                    })
            },
            // The worker may have started though its answer was lost: look
            // for it a few times, as the app may still be getting to it.
            |session| {
                for look in 0..OWN_WAIT_LOOKS {
                    if look > 0 {
                        std::thread::sleep(OWN_WAIT_PAUSE);
                    }
                    if let Ok(panels) = session.client.list_panels()
                        && let Some(pane) =
                            recovered_child(&earlier, &panels, own, wanted_label.as_deref())
                    {
                        return Some(pane);
                    }
                }
                None
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
            // Any listed worker may now be on the board (a new board takes
            // back the ones an earlier board handed back), so it is only
            // closed while this pane has none.
            |session, board| {
                let workers = session.client.list_panels().map(|panels| {
                    panels
                        .iter()
                        .any(|panel| panel.parent_pane_id == Some(WireId(own)))
                });
                if let Ok(false) = workers
                    && let Err(err) = session.client.close_pane(board)
                {
                    eprintln!("Noa +ctl: could not close the board this spawn opened: {err}");
                }
            },
        )?;
        let mut line = spawn_line(
            spawned.pane,
            spawned.recovered,
            spawned
                .made
                .as_ref()
                .map(|(_, made)| (made.path.as_path(), made.branch.as_str())),
            args.tier.map(worktree::Tier::name),
            result_path.as_deref(),
            started_in.as_deref(),
        );
        if let Some(fields) = line.as_object_mut() {
            fields.extend(extra);
        }
        let mut event = serde_json::json!({
            "pane": spawned.pane,
            "label": wanted_label,
            "argv": argv,
            "tier": args.tier.map(worktree::Tier::name),
            "cwd": started_in,
            "worktree": line.get("worktree"),
            "branch": line.get("branch"),
            "result": result_path,
        });
        if let Some(of) = line.get("retryOf") {
            event["retryOf"] = of.clone();
        }
        self.journal.append("spawn", event);
        let Some(limit) = args.ready.or(task.is_some().then_some(ready::TASK_READY)) else {
            print_json(&line);
            return Ok(EXIT_OK);
        };
        let pane = spawned.pane;
        // The worker runs whatever happens next: its line is printed.
        let (ready, screen) = match ready::wait_ready(self, pane, limit) {
            Ok(seen) => seen,
            Err(failure) => {
                print_json(&line);
                return Err(failure);
            }
        };
        line["ready"] = ready.name().into();
        line["screen"] = screen
            .as_deref()
            .map_or_else(String::new, |screen| last_lines(screen, PROMPT_LINES))
            .into();
        let Some((text, wait)) = task else {
            print_json(&line);
            return Ok(EXIT_OK);
        };
        if !ready::takes_task(ready, screen.as_deref()) {
            line["sent"] = false.into();
            print_json(&line);
            return Ok(EXIT_OK);
        }
        // Registered before the task is typed, so the header names the run.
        let run = self.register_run(pane, new_run);
        if let Some(run) = &run
            && let (Some(fields), serde_json::Value::Object(ids)) =
                (line.as_object_mut(), run_attr::journal_fields(run))
        {
            fields.extend(ids);
        }
        let text = run_attr::with_header(run.as_ref(), &text);
        let send = SendArgs {
            pane,
            text: TextArg::Literal(text.clone()),
            enter: true,
            raw: false,
            any_pane: false,
            answer: false,
            replace: false,
            queue: false,
            new_run: false,
            wait: Some(wait),
        };
        let mut sent = false;
        let delivered = self.deliver(&send, &text, || {
            line["sent"] = true.into();
            print_json(&line);
            sent = true;
        });
        if !sent {
            line["sent"] = false.into();
            print_json(&line);
        }
        delivered
    }

    /// `retry PANE`: [`retry::plan`] from the journal and one listing, then
    /// [`retry::steps`]: check the spawn as `run` checks one, close the old
    /// pane (journaled, not printed, never pruned), and spawn with the task.
    fn retry(&mut self, args: RetryArgs) -> Result<i32, Failure> {
        let given = args.task.as_ref().map(TaskText::read).transpose()?;
        let own = self.require_own()?;
        let panels = self.client.list_panels()?;
        let workers = journal::workers(&self.journal.events());
        let plan = retry::plan(
            &args,
            given,
            &workers,
            &panels,
            own,
            self.client.supports_board_timeline(),
            |path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0),
        )
        .map_err(Failure::error)?;
        let of_run = run_attr::retry_of(&mut self.client, &workers, plan.old, plan.listed);
        retry::steps(
            self,
            plan,
            |_, spawn| {
                let env = worker_env(Vec::new(), true, std::env::vars());
                check_spawn(
                    spawn,
                    std::env::current_dir().ok(),
                    env.get("PATH").map(String::as_str),
                )
                .map_err(|message| {
                    Failure::error(format!(
                        "the new worker cannot start: {message} (its directory is the old \
                         worker's). Nothing was done"
                    ))
                })
            },
            |session, old| {
                let journal = &mut session.journal;
                let code = close_panes(
                    &mut session.client,
                    &[old],
                    Some(own),
                    &[],
                    &mut |_, _, _| None,
                    &mut |line| {
                        if let Some(pane) = line_pane(line) {
                            journal.append(
                                "close",
                                serde_json::json!({ "pane": pane, "worktree": null }),
                            );
                        }
                    },
                )?;
                if code == EXIT_OK {
                    Ok(())
                } else {
                    Err(Failure {
                        code,
                        message: format!("pane {old} was not closed, so no worker was started"),
                        transient: false,
                    })
                }
            },
            |session, spawn, extra| session.spawn(spawn, extra, run_attr::NewRun::Retry { of_run }),
        )
    }

    fn send(&mut self, args: SendArgs) -> Result<i32, Failure> {
        self.guard(&[args.pane], args.any_pane)?;
        let text = match &args.text {
            TextArg::Literal(text) => text.clone(),
            TextArg::Stdin => {
                let mut text = String::new();
                std::io::stdin()
                    .read_to_string(&mut text)
                    .map_err(|err| Failure::error(format!("cannot read stdin: {err}")))?;
                text
            }
        };
        let text = if args.new_run {
            let supports_timeline = self.client.supports_board_timeline();
            new_run_text(self, &args, &text, supports_timeline, |session| {
                session.register_run(args.pane, run_attr::NewRun::SendNewRun)
            })?
        } else {
            text
        };
        self.deliver(&args, &text, || {})
    }

    /// Registers a timeline run for `worker` (`docs/specs/board-timeline.md`
    /// AC-4) and journals its ids as a `run` event. `None` when the server
    /// has no timeline, or when registering failed: then the task is still
    /// typed, without a header, and stderr says the worker cannot publish.
    fn register_run(
        &mut self,
        worker: u64,
        new_run: run_attr::NewRun,
    ) -> Option<run_attr::RunAttr> {
        let request_id = run_attr::new_request_id();
        match run_attr::register(&mut self.client, self.own, worker, new_run, &request_id) {
            Ok(run) => {
                if let Some(run) = &run {
                    let mut event = serde_json::json!({ "pane": worker });
                    if let (Some(event), serde_json::Value::Object(ids)) =
                        (event.as_object_mut(), run_attr::journal_fields(run))
                    {
                        event.extend(ids);
                    }
                    self.journal.append("run", event);
                }
                run
            }
            Err(kind) => {
                eprintln!("timeline: run not registered ({kind}): this worker cannot publish");
                None
            }
        }
    }

    /// `send`'s typing and what follows it: journal the send, then wait
    /// (`--wait`) or print the report count. `typed` runs once the text is
    /// in, before any wait line.
    fn deliver(
        &mut self,
        args: &SendArgs,
        text: &str,
        typed: impl FnOnce(),
    ) -> Result<i32, Failure> {
        // What an answer answered, for `status --answers`.
        let screen = if args.answer {
            client_text(&mut self.client, args.pane, TextSource::Screen)
                .ok()
                .flatten()
                .map(|text| last_lines(&text, PROMPT_LINES))
        } else {
            None
        };
        let since = SystemTime::now();
        let before = send_steps(self, args, text)?;
        let seq = before.agent.as_ref().map_or(0, |agent| agent.seq);
        let mut event = serde_json::json!({
            "pane": args.pane,
            "seqBefore": seq,
            "text": journal::clip(text),
            "bytes": text.len(),
            "enter": args.enter,
            "raw": args.raw,
            "answer": args.answer,
            "queue": args.queue,
        });
        if let Some(full) = journal::full(text) {
            event["full"] = full.into();
        }
        if let Some(screen) = screen {
            event["screen"] = screen.into();
        }
        self.journal.append("send", event);
        typed();
        match &args.wait {
            Some(options) => {
                // Said before the wait, so a wait its shell tool kills
                // without a timeout line still leaves the way back.
                eprintln!("{}", resume_hint(args.pane, seq));
                let results = self.result_paths(std::slice::from_ref(&before));
                self.wait_subscribed(
                    &[args.pane],
                    send_wait_start(before, seq),
                    options,
                    (since, HashMap::new(), results),
                )
            }
            None => {
                print_json(&serde_json::json!({ "paneId": WireId(args.pane), "seq": seq }));
                Ok(EXIT_OK)
            }
        }
    }

    /// `noa.sendText`, with a refusal of a pane whose process has exited
    /// (held open, so still listed) said as such rather than "pane closed".
    /// Returns `false` for a paste that `requirePasteMode` refused (-32010):
    /// nothing was written.
    fn send_input(
        &mut self,
        pane: u64,
        text: &str,
        options: SendTextOptions,
    ) -> Result<bool, Failure> {
        match self.client.send_text_with(pane, text, options) {
            Err(ClientError::Rpc {
                code: RPC_NOT_READY,
                ..
            }) if options.require_paste_mode => Ok(false),
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
            Err(ClientError::Rpc {
                code: RPC_OUTCOME_UNKNOWN,
                message,
            }) => Err(Failure::unavailable(format!(
                "Noa did not confirm the input to pane {pane} in time ({message}); it may \
                 already be typed: `read` the pane before sending again"
            ))),
            result => result.map(|()| true).map_err(Failure::from),
        }
    }

    /// `wait`, with `guard` for [`Self::guard_wait`]. Its listings before
    /// the loop are taken as the loop's first one is, with the terminating
    /// signals already caught and within the same `--timeout`.
    fn wait(
        &mut self,
        panes: &[u64],
        named: bool,
        guard: bool,
        after: &[(u64, u64)],
        options: &WaitOptions,
    ) -> Result<i32, Failure> {
        let deadline = options
            .timeout
            .and_then(|timeout| Instant::now().checked_add(timeout));
        // What an interrupted line can say before the wait has a listing.
        let known = WaitStart::Listed {
            after: after.iter().copied().collect(),
        };
        let print = &mut |line: &serde_json::Value| print_json(line);
        with_signals_caught(|| {
            if guard && !self.guard_wait(panes, &known, deadline)? {
                return Ok(EXIT_TIMEOUT);
            }
            self.client
                .subscribe(&[EventKind::StateChanged], Some(panes))?;
            let mut panes = panes.to_vec();
            if !named && after.iter().any(|(pane, _)| !panes.contains(pane)) {
                let mut pending = panes.clone();
                pending.extend(closed_after_panes(&panes, after, &[]));
                let Some(listed) =
                    list_before_wait(&mut self.client, &known, &pending, deadline, print)?
                else {
                    return Ok(EXIT_TIMEOUT);
                };
                let listed: Vec<u64> = listed.iter().map(|panel| panel.pane_id.0).collect();
                panes.extend(closed_after_panes(&panes, after, &listed));
            }
            let after = wait_baselines(&panes, after).map_err(Failure::error)?;
            let resumed = resumed_since(&self.journal.events(), &after);
            let start = if named {
                WaitStart::Named { after }
            } else {
                WaitStart::Listed { after }
            };
            let since = SystemTime::now();
            let Some(panels) = list_before_wait(&mut self.client, &start, &panes, deadline, print)?
            else {
                return Ok(EXIT_TIMEOUT);
            };
            let results = self.result_paths(&panels);
            // The loop's `--timeout` is what is left of the wait's.
            let options = WaitOptions {
                timeout: deadline
                    .map(|deadline| deadline.saturating_duration_since(Instant::now())),
                ..options.clone()
            };
            self.wait_subscribed(&panes, start, &options, (since, resumed, results))
        })
    }

    /// Each pane's known result file ([`result_path`]).
    fn result_paths(&self, panels: &[Panel]) -> HashMap<u64, PathBuf> {
        result_paths(&journal::workers(&self.journal.events()), panels)
    }

    /// The wait loop, printing each line with its pane's result file
    /// (`results`) once written after `since`, or for a pane in `resumed`
    /// after its own time there, and journaling each outcome.
    fn wait_subscribed(
        &mut self,
        panes: &[u64],
        start: WaitStart,
        options: &WaitOptions,
        (since, resumed, results): (SystemTime, HashMap<u64, SystemTime>, HashMap<u64, PathBuf>),
    ) -> Result<i32, Failure> {
        let journal = &mut self.journal;
        let client = &mut self.client;
        with_signals_caught(|| {
            wait_loop(
                client,
                panes,
                start,
                options,
                WAIT_RELIST_INTERVAL,
                &mut |line| {
                    let mut line = line.clone();
                    if let Some(event) = finish_wait_line(&mut line, &results, since, &resumed) {
                        journal.append("outcome", event);
                    }
                    print_json(&line);
                },
            )
        })
    }

    /// `status`: one line per worker of this pane, with what the journal
    /// knows; workers the journal knows that are gone come last.
    /// `--answers`: every prompt answered from here, oldest first.
    fn status(&mut self, answers: bool) -> Result<i32, Failure> {
        let events = self.journal.events();
        if answers {
            journal::answers(&events).iter().for_each(print_json);
            return Ok(EXIT_OK);
        }
        let children = self.children()?;
        let here = std::env::current_dir().ok();
        let lines = status_lines(&children, &journal::workers(&events), |branch, dir| {
            let ahead = here
                .as_deref()
                .and_then(|here| worktree::ahead(here, branch));
            (ahead, worktree::dirty(Path::new(dir)))
        });
        lines.iter().for_each(print_json);
        Ok(EXIT_OK)
    }

    /// Where `result PANE` reads: the journal's path for the pane's spawn,
    /// else the labelled file of a pane `ls` lists.
    fn result_file(&mut self, pane: u64) -> Result<PathBuf, Failure> {
        let workers = journal::workers(&self.journal.events());
        let given = workers
            .iter()
            .find(|worker| worker.pane == pane)
            .and_then(|worker| worker.result.clone());
        match given {
            Some(path) => Ok(PathBuf::from(path)),
            None => {
                let panels = self.client.list_panels()?;
                panels
                    .iter()
                    .find(|panel| panel.pane_id.0 == pane)
                    .and_then(|panel| result_path(&workers, panel))
                    .ok_or_else(|| Failure::error(no_known_result(pane)))
            }
        }
    }

    /// `result PANE`: print the worker's result file, or with `path_only`
    /// just its path.
    fn result(
        &mut self,
        pane: u64,
        max_bytes: Option<usize>,
        path_only: bool,
    ) -> Result<i32, Failure> {
        let path = self.result_file(pane)?;
        if path_only {
            let _ = result::write_path(&mut std::io::stdout().lock(), &path);
            return Ok(EXIT_OK);
        }
        let (text, cut) = result::read(pane, &path, max_bytes).map_err(Failure::error)?;
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(text.as_bytes());
        if !text.ends_with('\n') {
            let _ = stdout.write_all(b"\n");
        }
        if cut {
            eprintln!(
                "Noa +ctl: result cut; printed the first {} bytes of {}",
                text.len(),
                path.display()
            );
        }
        Ok(EXIT_OK)
    }
}

impl ready::ReadySource for Session {
    fn panel(&mut self, pane: u64) -> Result<Option<Panel>, Failure> {
        SendTarget::panel(self, pane)
    }

    fn screen(&mut self, pane: u64) -> Result<Option<String>, Failure> {
        client_text(&mut self.client, pane, TextSource::Screen)
    }

    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

impl SendTarget for Session {
    fn panel(&mut self, pane: u64) -> Result<Option<Panel>, Failure> {
        Ok(self
            .client
            .list_panels()?
            .into_iter()
            .find(|panel| panel.pane_id.0 == pane))
    }

    fn subscribe(&mut self, pane: u64) -> Result<(), Failure> {
        self.client
            .subscribe(&[EventKind::StateChanged], Some(&[pane]))?;
        Ok(())
    }

    fn type_text(&mut self, pane: u64, text: &str, paste: bool) -> Result<(), Failure> {
        let options = SendTextOptions {
            paste,
            require_paste_mode: false,
            refuse_shell: false,
        };
        self.send_input(pane, text, options).map(drop)
    }

    /// An empty paste that requires paste mode: refused (-32010) while the
    /// pane has not enabled bracketed paste, and writes nothing either way.
    /// A server that predates the option takes it as ready.
    fn paste_ready(&mut self, pane: u64) -> Result<bool, Failure> {
        let options = SendTextOptions {
            paste: true,
            require_paste_mode: true,
            refuse_shell: false,
        };
        self.send_input(pane, "", options)
    }

    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// What [`send_steps`] talks to.
pub(crate) trait SendTarget {
    /// `pane`'s panel; `None` when it is not listed.
    fn panel(&mut self, pane: u64) -> Result<Option<Panel>, Failure>;
    /// Subscribe to `pane`'s state changes (`send --wait`).
    fn subscribe(&mut self, pane: u64) -> Result<(), Failure>;
    fn type_text(&mut self, pane: u64, text: &str, paste: bool) -> Result<(), Failure>;
    /// Whether `pane` takes a paste as one (bracketed paste is on); writes
    /// nothing.
    fn paste_ready(&mut self, pane: u64) -> Result<bool, Failure>;
    fn pause(&mut self, duration: Duration);
}

/// How often, and how long, `send` asks again whether a pane has enabled
/// bracketed paste: 200 ms apart for up to 15 s.
const PASTE_MODE_PAUSE: Duration = Duration::from_millis(200);
const PASTE_MODE_TRIES: usize = 75;

/// Whether a paste into `panel` waits until it has enabled bracketed paste
/// (REL-01): an agent's pane, or one whose program is not even listed yet
/// (just spawned). Until then every newline of a paste lands as Enter.
///
/// A known spawn command may be a script or REPL that never enables
/// bracketed paste. For older servers that omit the kind before first
/// output, use the same executable classifier as the session store.
pub(crate) fn needs_paste_mode(panel: &Panel) -> bool {
    use crate::session_store::AgentKind;
    panel
        .agent
        .as_ref()
        .is_some_and(|agent| agent.kind.is_some())
        || panel
            .spawn
            .as_ref()
            .and_then(|spawn| spawn.argv.first())
            .is_some_and(|argv0| AgentKind::from_spawn(argv0) != AgentKind::Generic)
        || (panel.process.as_deref().is_none_or(str::is_empty) && panel.spawn.is_none())
}

/// Wait for `pane` to enable bracketed paste, asking every
/// [`PASTE_MODE_PAUSE`] up to [`PASTE_MODE_TRIES`] times.
fn wait_for_paste_mode(target: &mut impl SendTarget, pane: u64) -> Result<(), Failure> {
    for tried in 0..=PASTE_MODE_TRIES {
        if target.paste_ready(pane)? {
            return Ok(());
        }
        if tried < PASTE_MODE_TRIES {
            target.pause(PASTE_MODE_PAUSE);
        }
    }
    Err(Failure::error(format!(
        "pane {pane} has not enabled bracketed paste yet (its program may still be starting); \
         nothing was sent"
    )))
}

/// Refuse a text too large for one request, before anything is typed.
pub(crate) fn check_send_size(text: &str) -> Result<(), String> {
    let encoded = serde_json::to_string(text)
        .expect("a string serializes")
        .len();
    if encoded > MAX_SEND_TEXT_BYTES {
        return Err(format!(
            "the text is {encoded} bytes after JSON encoding; one send takes at most {} KiB. \
             Nothing was sent: write the text to a file and send its path",
            MAX_SEND_TEXT_BYTES / 1024
        ));
    }
    Ok(())
}

/// Why `send` refuses `panel` before typing, if it does. Enter approves some
/// prompts with their default choice, and picks the first choice of a
/// question (D7), so either is only answered on purpose (`--answer`). A
/// working agent would queue pasted text or a bare Enter, and a `--wait`
/// could end with the turn already running (`--queue` takes that); `--raw`
/// keys (Esc, Ctrl-C) are how it is interrupted, so they always go.
pub(crate) fn send_refusal(panel: &Panel, args: &SendArgs, text: &str) -> Option<String> {
    if args.answer {
        return None;
    }
    if let Some(awaited) = awaits_answer(panel) {
        return Some(format!(
            "pane {} is waiting for {awaited}; read it and pass --answer. Nothing was sent.",
            args.pane
        ));
    }
    let working = panel
        .agent
        .as_ref()
        .is_some_and(|agent| agent.state.as_deref() == Some("running"));
    let keys = args.raw && !text.is_empty();
    if working && !args.queue && !keys {
        // `--new-run` does not go with `--queue`: a new run waits.
        let queue = if args.new_run {
            ""
        } else {
            ", or pass --queue (the wait may then end with the current turn)"
        };
        return Some(format!(
            "pane {} is working; wait for its turn to end{queue}",
            args.pane
        ));
    }
    None
}

/// The stderr line `send --wait` prints once the text is in: how to carry
/// on if this command is killed before its wait ends.
pub(crate) fn resume_hint(pane: u64, seq: u64) -> String {
    format!("Noa +ctl: sent to pane {pane}; resume with: wait {pane} --after {seq}")
}

/// `send`'s refusals that are known before anything is typed: a text too
/// large for one request, an unknown pane, one that should not take the
/// text ([`send_refusal`]), and one whose program has not enabled
/// bracketed paste in time. Writes nothing; returns the panel as it was.
pub(crate) fn send_checks(
    target: &mut impl SendTarget,
    args: &SendArgs,
    text: &str,
) -> Result<Panel, Failure> {
    check_send_size(text).map_err(Failure::error)?;
    let before = target
        .panel(args.pane)?
        .ok_or_else(|| Failure::error("unknown pane"))?;
    if let Some(refusal) = send_refusal(&before, args, text) {
        return Err(Failure::error(refusal));
    }
    // Before `--replace`'s keys too: they would reach a program that is
    // still starting.
    if !args.raw && !text.is_empty() && needs_paste_mode(&before) {
        wait_for_paste_mode(target, args.pane)?;
    }
    Ok(before)
}

/// `send --new-run`'s text: the header of the run `register` registers,
/// then `text` (`text` alone when no run was registered). [`send_checks`]
/// runs first, counting the header on timeline-capable servers, so a send
/// that would be refused registers and journals no run (board-timeline
/// AC-4). [`send_steps`] checks again when it types.
pub(crate) fn new_run_text<T: SendTarget>(
    target: &mut T,
    args: &SendArgs,
    text: &str,
    supports_timeline: bool,
    register: impl FnOnce(&mut T) -> Option<run_attr::RunAttr>,
) -> Result<String, Failure> {
    send_checks(
        target,
        args,
        &run_attr::prospective_text(text, supports_timeline),
    )?;
    if !supports_timeline {
        return Ok(text.to_owned());
    }
    let run = register(target);
    Ok(run_attr::with_header(run.as_ref(), text))
}

/// `send`'s checks and typing, before any wait: [`send_checks`], subscribe
/// for `--wait`, clear the input line for `--replace`, type, and press
/// Enter. Returns the panel as it was before typing.
pub(crate) fn send_steps(
    target: &mut impl SendTarget,
    args: &SendArgs,
    text: &str,
) -> Result<Panel, Failure> {
    let before = send_checks(target, args, text)?;
    // Subscribe before typing, so the reply cannot arrive unobserved.
    if args.wait.is_some() {
        target.subscribe(args.pane)?;
    }
    if args.replace {
        for keys in replace_keys() {
            target.type_text(args.pane, &keys, false)?;
            target.pause(REPLACE_PAUSE);
        }
        // Let the TUI take the keys before a paste arrives.
        target.pause(SUBMIT_DELAY);
        // Clearing takes long enough for an approval to come up meanwhile.
        // `--replace` never goes with `--answer`.
        if let Some(awaited) = target.panel(args.pane)?.as_ref().and_then(awaits_answer) {
            return Err(Failure::error(format!(
                "pane {} began waiting for {awaited} while its input line was cleared; the \
                 text was not typed.",
                args.pane
            )));
        }
    }
    if !text.is_empty() {
        target.type_text(args.pane, text, !args.raw)?;
    }
    if args.enter {
        if !text.is_empty() {
            target.pause(SUBMIT_DELAY);
            let asked = target.panel(args.pane)?.as_ref().and_then(awaits_answer);
            if !args.answer
                && let Some(awaited) = asked
            {
                return Err(Failure::error(format!(
                    "pane {} began waiting for {awaited} after the text was typed; Enter was \
                     not sent, so the text may still be in its input line.",
                    args.pane
                )));
            }
        }
        target.type_text(args.pane, "\r", false)?;
    }
    Ok(before)
}

/// Where a spawn's worker goes (`pane`), and whether this spawn opened that
/// pane as the board.
pub(crate) struct Host {
    pub(crate) pane: u64,
    pub(crate) opened_board: bool,
}

/// What [`spawn_steps`] started.
#[derive(Debug, PartialEq)]
pub(crate) struct Spawned<W> {
    pub(crate) pane: u64,
    pub(crate) made: Option<W>,
    /// The spawn's answer was lost and the worker was found by listing.
    pub(crate) recovered: bool,
}

/// A spawn's steps once its checks passed: make the `--worktree` worktree
/// (`worktree`, `None` without one), find the host pane (`host`, which opens
/// the board), start the worker there (`spawn`), and clean up when the start
/// failed.
///
/// The worktree comes first: git refusing it (the branch or directory
/// exists) then starts nothing, not even the board. After a request the app
/// did not confirm in time (-32009) or a client timeout, the worker may be
/// running: `recover` looks for it, and when found the spawn succeeds.
/// Otherwise `undo` is told whether the server refused the spawn (an RPC
/// error other than -32009), the one case where no worker can be running in
/// the worktree, and only that case closes a board this spawn opened
/// (`close_board`, which checks the board holds no worker).
pub(crate) fn spawn_steps<S, W>(
    session: &mut S,
    worktree: impl FnOnce(&mut S) -> Result<Option<W>, Failure>,
    host: impl FnOnce(&mut S) -> Host,
    spawn: impl FnOnce(&mut S, u64, Option<&W>) -> Result<u64, ClientError>,
    recover: impl FnOnce(&mut S) -> Option<u64>,
    undo: impl FnOnce(&mut S, &W, bool),
    close_board: impl FnOnce(&mut S, u64),
) -> Result<Spawned<W>, Failure> {
    let made = worktree(session)?;
    let host = host(session);
    let err = match spawn(session, host.pane, made.as_ref()) {
        Ok(pane) => {
            return Ok(Spawned {
                pane,
                made,
                recovered: false,
            });
        }
        Err(err) => err,
    };
    let unknown = matches!(
        err,
        ClientError::Rpc {
            code: RPC_OUTCOME_UNKNOWN,
            ..
        } | ClientError::Timeout(_)
    );
    if unknown && let Some(pane) = recover(session) {
        return Ok(Spawned {
            pane,
            made,
            recovered: true,
        });
    }
    let refused = matches!(err, ClientError::Rpc { code, .. } if code != RPC_OUTCOME_UNKNOWN);
    if let Some(made) = &made {
        undo(session, made, refused);
    }
    if refused && host.opened_board {
        close_board(session, host.pane);
    }
    Err(err.into())
}

/// The worker a spawn whose answer was lost started: the one child of `own`
/// in `panels` that is not among `earlier` (and has `label`, when the spawn
/// gave one). `None` when there is none, or more than one to choose from.
pub(crate) fn recovered_child(
    earlier: &[u64],
    panels: &[Panel],
    own: u64,
    label: Option<&str>,
) -> Option<u64> {
    let mut new = panels.iter().filter(|panel| {
        panel.parent_pane_id == Some(WireId(own))
            && !earlier.contains(&panel.pane_id.0)
            && label.is_none_or(|label| panel.label.as_deref() == Some(label))
    });
    match (new.next(), new.next()) {
        (Some(panel), None) => Some(panel.pane_id.0),
        _ => None,
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
    /// Whether a terminating signal arrived, so the wait ends as a timeout.
    fn interrupted(&mut self) -> bool {
        false
    }
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

    fn interrupted(&mut self) -> bool {
        board::SIGNALED.load(std::sync::atomic::Ordering::Relaxed) != 0
    }
}

/// Run `f` with the terminating signals caught (the board's handler), so
/// a wait that its shell tool kills still prints the timeout line with the
/// baselines to carry on from. A signal ignored on entry (a shell's `&` job
/// ignores SIGINT) stays ignored.
fn with_signals_caught<T>(f: impl FnOnce() -> T) -> T {
    let previous = board::TERMINATING_SIGNALS.map(|signal| {
        // SAFETY: the handler only stores to an atomic.
        let previous = unsafe {
            libc::signal(
                signal,
                board::on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            )
        };
        if previous == libc::SIG_IGN {
            // SAFETY: puts back the disposition the process started with.
            unsafe { libc::signal(signal, libc::SIG_IGN) };
        }
        previous
    });
    let result = f();
    for (signal, previous) in board::TERMINATING_SIGNALS.into_iter().zip(previous) {
        // SAFETY: back to the disposition found on entry.
        unsafe { libc::signal(signal, previous) };
    }
    result
}

/// `None` when the pane closed between its outcome and this request.
fn client_text(
    client: &mut Client,
    pane: u64,
    source: TextSource,
) -> Result<Option<String>, Failure> {
    match client.get_text(pane, source, None) {
        Ok(result) => {
            if let Some(notice) = history_notice(pane, source, &result) {
                eprintln!("{notice}");
            }
            Ok(Some(result.text))
        }
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

/// A `finished` held by `--settle` (AU-12): its report count, since when,
/// and the work on screen (above the input box) when the hold began.
struct Hold {
    seq: u64,
    since: Instant,
    work: Option<String>,
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
    let emit = &mut |line: &serde_json::Value| {
        if options.queued {
            let mut line = line.clone();
            line["queued"] = true.into();
            emit(&line);
        } else {
            emit(line);
        }
    };
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
    let Some(first) = list_before_wait(source, &start, panes, deadline, emit)? else {
        return Ok(EXIT_TIMEOUT);
    };
    let mut latest = watched(first, panes);
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
    let mut holds: HashMap<u64, Hold> = HashMap::new();
    let mut holds_polled_at: Option<Instant> = None;
    loop {
        for watch in &mut watches {
            if done.contains(&watch.pane) || (skip_exited_before && watch.exited_before) {
                continue;
            }
            let panel = latest.get(&watch.pane);
            match evaluate(watch, panel, &options.reasons) {
                // `--settle`: a `finished` is held, not printed yet. The
                // same report seen again keeps its hold.
                Some(outcome) if outcome.reason == Reason::Finished && options.settle.is_some() => {
                    let seq = panel
                        .and_then(|panel| panel.agent.as_ref())
                        .map_or(0, |agent| agent.seq);
                    if holds.get(&watch.pane).is_none_or(|hold| hold.seq != seq) {
                        let work = match source.screen(watch.pane) {
                            Ok(Some(text)) => Some(prompt::work(&text)),
                            Ok(None) => None,
                            Err(failure) if failure.transient => None,
                            Err(failure) => return Err(failure),
                        };
                        holds.insert(
                            watch.pane,
                            Hold {
                                seq,
                                since: clock(),
                                work,
                            },
                        );
                    }
                }
                Some(outcome) => {
                    holds.remove(&watch.pane);
                    emit_outcome(source, options, watch, &outcome, panel, None, emit);
                    done.push(watch.pane);
                }
                // A newer report that ends nothing (`running`): the agent
                // went on. Drop the held `finished`; a later report is
                // judged as usual.
                None => {
                    if let Some(hold) = holds.remove(&watch.pane) {
                        watch.baseline_seq = hold.seq;
                    }
                }
            }
        }
        // A held `finished` stands once its hold passed with the work on
        // screen unchanged. A change starts the hold again and keeps the
        // report: an agent that goes on keeps changing its screen or reports
        // again, while a one-off repaint (a late render, a re-wrap) only
        // delays the line. Dropping it hung a wait with no newer report.
        if let Some(settle) = options.settle
            && !holds.is_empty()
            && !relist_pending
        {
            let now = clock();
            if holds_polled_at.is_none_or(|at| now.saturating_duration_since(at) >= relist_every) {
                holds_polled_at = Some(now);
                let mut settled: Vec<u64> = Vec::new();
                for watch in &mut watches {
                    let Some(hold) = holds.get_mut(&watch.pane) else {
                        continue;
                    };
                    let work = match source.screen(watch.pane) {
                        Ok(Some(text)) => prompt::work(&text),
                        Ok(None) => continue,
                        Err(failure) if failure.transient => continue,
                        Err(failure) => return Err(failure),
                    };
                    if hold.work.as_ref().is_some_and(|seen| *seen != work) {
                        hold.since = now;
                        hold.work = Some(work);
                        continue;
                    }
                    hold.work = Some(work);
                    if now.saturating_duration_since(hold.since) >= settle {
                        settled.push(watch.pane);
                    }
                }
                if !settled.is_empty() {
                    // Reports received while the screens were read come
                    // first: one newer than the held `finished` is judged on
                    // the next pass instead.
                    let mut relist = false;
                    while let Some(notification) = source.next(Duration::ZERO)? {
                        relist |= apply_notification(&mut latest, notification, panes);
                    }
                    if relist {
                        relist_pending = true;
                        settled.clear();
                    }
                }
                for pane in settled {
                    let Some(watch) = watches.iter_mut().find(|watch| watch.pane == pane) else {
                        continue;
                    };
                    let panel = latest.get(&pane);
                    let seq = panel
                        .and_then(|panel| panel.agent.as_ref())
                        .map_or(0, |agent| agent.seq);
                    // Anything else that changed is judged on the next pass.
                    if holds.get(&pane).is_none_or(|hold| hold.seq != seq) {
                        continue;
                    }
                    if let Some(outcome) = evaluate(watch, panel, &options.reasons)
                        && outcome.reason == Reason::Finished
                    {
                        holds.remove(&pane);
                        let secs = secs_json(settle);
                        emit_outcome(source, options, watch, &outcome, panel, None, &mut |line| {
                            let mut line = line.clone();
                            line["settledSecs"] = secs.clone();
                            emit(&line);
                        });
                        done.push(pane);
                    }
                }
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
                    if done.contains(&watch.pane)
                        || (skip_exited_before && watch.exited_before)
                        || holds.contains_key(&watch.pane)
                    {
                        continue;
                    }
                    // A screen read that timed out is taken on the next poll.
                    let text = match source.screen(watch.pane) {
                        Ok(Some(text)) => text,
                        Ok(None) => continue,
                        Err(failure) if failure.transient => continue,
                        Err(failure) => return Err(failure),
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
                        match source.list() {
                            Ok(panels) => {
                                latest = watched(panels, panes);
                                listed_at = Instant::now();
                            }
                            // Judged only on a fresh listing: the stalled
                            // panes are taken again on the next poll.
                            Err(failure) if failure.transient => {
                                relist_pending = true;
                                stalled.clear();
                            }
                            Err(failure) => return Err(failure),
                        }
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
                                    agent_left: false,
                                },
                                Some(quiet_for),
                            ),
                        };
                        emit_outcome(source, options, watch, &outcome, panel, quiet, emit);
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
            match source.list() {
                Ok(panels) => {
                    latest = watched(panels, panes);
                    listed_at = Instant::now();
                    relist_pending = false;
                    continue;
                }
                // Still pending: re-listed on the next tick, or the wait
                // times out below.
                Err(failure) if failure.transient => {}
                Err(failure) => return Err(failure),
            }
        }
        // Checked between requests: a caught signal lets the one in flight
        // finish (each is bounded), then ends the wait as a timeout would.
        if source.interrupted() {
            let mut line = timeout_json(&watches, &done, &latest);
            line["interrupted"] = true.into();
            emit(&line);
            return Ok(EXIT_TIMEOUT);
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
        // Still set here only when a re-list above timed out: keep it.
        relist_pending |= relist || listed_at.elapsed() >= relist_every;
    }
}

/// A listing a wait takes before it has one (its first, the named wait's
/// guard, the result paths). A request past the client's deadline on a live
/// connection is retried until the wait's own `deadline`; a lost connection
/// still ends the wait. `None` when a caught signal ended the wait first
/// (a wedged Noa keeps the socket open), after its line went to `emit`.
pub(crate) fn list_before_wait(
    source: &mut impl WaitSource,
    start: &WaitStart,
    panes: &[u64],
    deadline: Option<Instant>,
    emit: &mut dyn FnMut(&serde_json::Value),
) -> Result<Option<Vec<Panel>>, Failure> {
    loop {
        match source.list() {
            Err(failure)
                if failure.transient
                    && deadline.is_none_or(|deadline| Instant::now() < deadline) =>
            {
                if source.interrupted() {
                    emit(&interrupted_before_listing(start, panes));
                    return Ok(None);
                }
            }
            result => return result.map(Some),
        }
    }
}

/// The line of a wait a signal ended before any listing came back: every
/// pane pending, with the baselines known without a listing (`--after`, or
/// what `send --wait` saw before typing).
fn interrupted_before_listing(start: &WaitStart, panes: &[u64]) -> serde_json::Value {
    let known = |pane: u64| match start {
        WaitStart::Listed { after } | WaitStart::Named { after } => after.get(&pane).copied(),
        WaitStart::Before { panels, after } => after.or_else(|| {
            panels
                .iter()
                .find(|panel| panel.pane_id.0 == pane)
                .and_then(|panel| Some(panel.agent.as_ref()?.seq))
        }),
    };
    serde_json::json!({
        "timeout": true,
        "interrupted": true,
        "pending": panes.iter().copied().map(WireId).collect::<Vec<_>>(),
        "baseline": panes
            .iter()
            .filter_map(|&pane| Some(serde_json::json!({ "paneId": WireId(pane), "seq": known(pane)? })))
            .collect::<Vec<_>>(),
    })
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
        Notification::StateChanged {
            panels, dropped, ..
        } => {
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
/// The wait's outcome stands even when the tail cannot be read: the line
/// then says why under `tailError` instead. A `permission` or `input` line
/// shows what is asked (`"prompt"`, AU-11); on an agent's pane, any other
/// line's tail is taken from above its input box.
fn emit_outcome(
    source: &mut impl WaitSource,
    options: &WaitOptions,
    watch: &Watch,
    outcome: &Outcome,
    panel: Option<&Panel>,
    quiet: Option<Duration>,
    emit: &mut dyn FnMut(&serde_json::Value),
) {
    let mut line = outcome_json(watch, outcome, panel, quiet);
    let asks = matches!(outcome.reason, Reason::Permission | Reason::Input);
    if asks
        && !outcome.closed
        && let Ok(Some(screen)) = source.screen(watch.pane)
        && let Some(asked) = prompt::block(&screen)
    {
        line["prompt"] = asked.into();
    }
    let agent = panel
        .and_then(|panel| panel.agent.as_ref())
        .is_some_and(|agent| agent.kind.is_some());
    if let Some(lines) = options.tail
        && !outcome.closed
    {
        match source.scrollback(watch.pane) {
            Ok(Some(text)) if agent && !asks => {
                line["tail"] = prompt::agent_tail(&text, lines).into();
            }
            Ok(Some(text)) => line["tail"] = tail_lines(&text, lines).into(),
            Ok(None) => {}
            Err(failure) => line["tailError"] = failure.message.into(),
        }
    }
    emit(&line);
}

/// The line a timed-out wait prints. A pane that had exited before the wait
/// (and so was never waited on) is under `exited`, not `pending`, and has no
/// baseline. The baselines of the pending panes, with each one's agent
/// state, let a later `wait --after` carry on without missing a report that
/// lands in between.
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
            .filter(|watch| !watch.exited_before)
            .map(|watch| {
                let mut entry = serde_json::json!({
                    "paneId": WireId(watch.pane),
                    "seq": watch.baseline_seq,
                });
                if let Some(label) = latest
                    .get(&watch.pane)
                    .and_then(|panel| panel.label.as_ref())
                    .or(watch.label.as_ref())
                {
                    entry["label"] = label.clone().into();
                }
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

/// Under `--children`, the panes `--after` names that are no longer listed
/// at all: workers that closed since the timeout line that gave their
/// baseline. They are waited on too, so they are reported like a worker
/// that goes away during the wait (`exit`, `closed`); a pane that is still
/// listed but is not a worker stays an error.
pub(crate) fn closed_after_panes(
    children: &[u64],
    after: &[(u64, u64)],
    listed: &[u64],
) -> Vec<u64> {
    let mut closed = Vec::new();
    for (pane, _) in after {
        if !children.contains(pane) && !listed.contains(pane) && !closed.contains(pane) {
            closed.push(*pane);
        }
    }
    closed
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

/// Refuse input to a pane that is not one of `own`'s workers. One that is
/// not listed at all is said as such: `--any-pane` would not help a
/// mistyped id.
pub(crate) fn guard_targets(panels: &[Panel], panes: &[u64], own: u64) -> Result<(), String> {
    for pane in panes {
        match panels.iter().find(|panel| panel.pane_id.0 == *pane) {
            None => return Err(format!("pane {pane} is not listed")),
            Some(panel) if panel.parent_pane_id != Some(WireId(own)) => {
                return Err(not_worker_message(*pane));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Refuse a wait on a listed pane that is not `own`'s worker (`own` itself
/// is fine). One that is not listed is left to the wait, which says so.
pub(crate) fn wait_guard(panels: &[Panel], panes: &[u64], own: u64) -> Result<(), String> {
    for pane in panes {
        if let Some(panel) = panels.iter().find(|panel| panel.pane_id.0 == *pane)
            && *pane != own
            && panel.parent_pane_id != Some(WireId(own))
        {
            return Err(not_worker_message(*pane));
        }
    }
    Ok(())
}

/// What `panel`'s agent waits for, if it is an answer, as the noun phrase a
/// refusal names: an approval (`permission`), or whatever an `OSC 7501`
/// blocked report names (`agent.blocked`: a question's dialog takes Enter
/// as its first choice). A plain `input` is not refused: from a hook
/// adapter's `idle_prompt`, or from a server that predates `blocked`, it
/// stands for an agent idle at its prompt, where a follow-up send is normal.
pub(crate) fn awaits_answer(panel: &Panel) -> Option<&'static str> {
    let agent = panel.agent.as_ref()?;
    match (agent.blocked.as_deref(), agent.state.as_deref()) {
        (Some("question"), _) => Some("an answer to a question"),
        (Some(_), _) | (None, Some("permission")) => Some("an approval"),
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

/// How many of `panels` are `own`'s workers, and how many of those exited
/// (held open, they keep their slot).
pub(crate) fn children_counts(panels: &[Panel], own: u64) -> (usize, usize) {
    panels
        .iter()
        .filter(|panel| panel.parent_pane_id == Some(WireId(own)))
        .fold((0, 0), |(all, exited), panel| {
            (all + 1, exited + usize::from(panel.exit.is_some()))
        })
}

/// A spawn the server refused at `maxChildren` (-32011), said as the
/// client's own check says it, from a fresh listing (`relist`; the server's
/// own message when that fails). Still an RPC error, so a `--worktree` spawn
/// counts it as refused and removes its worktree.
pub(crate) fn limit_reached_said(
    err: ClientError,
    own: u64,
    max: usize,
    relist: impl FnOnce() -> Option<Vec<Panel>>,
) -> ClientError {
    match err {
        ClientError::Rpc {
            code: RPC_LIMIT_REACHED,
            message,
        } => ClientError::Rpc {
            code: RPC_LIMIT_REACHED,
            message: match relist() {
                Some(panels) => {
                    let (workers, exited) = children_counts(&panels, own);
                    max_children_message(workers, exited, max)
                }
                None => message,
            },
        },
        other => other,
    }
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

/// The pane a printed line is about (`"paneId"`).
pub(crate) fn line_pane(line: &serde_json::Value) -> Option<u64> {
    line.get("paneId")?.as_str()?.parse().ok()
}

/// A listed pane's result file: what its spawn gave it (`workers`, from
/// the journal), else `<cwd>/.noa/result-<label>.md` for a pane with a plain
/// label. `None`: no known file (a shared name is never guessed).
pub(crate) fn result_path(workers: &[journal::Worker], panel: &Panel) -> Option<PathBuf> {
    workers
        .iter()
        .find(|worker| worker.pane == panel.pane_id.0)
        .and_then(|worker| worker.result.as_ref())
        .map(PathBuf::from)
        .or_else(|| result::labelled_path(Path::new(&panel.cwd), panel.label.as_deref()))
}

/// Why `result PANE` has no file to read.
pub(crate) fn no_known_result(pane: u64) -> String {
    format!(
        "pane {pane} has no known result file: this pane's journal has no spawn of it, and it \
         has no plain label to find <its cwd>/.noa/result-<label>.md by"
    )
}

/// [`result_path`] of each listed pane that has one.
pub(crate) fn result_paths(workers: &[journal::Worker], panels: &[Panel]) -> HashMap<u64, PathBuf> {
    panels
        .iter()
        .filter_map(|panel| Some((panel.pane_id.0, result_path(workers, panel)?)))
        .collect()
}

/// A wait line as printed: with `"result"` when its pane's result file was
/// written after `since`, or for a pane in `resumed` ([`resumed_since`])
/// after the send it resumes. Returns the journal's `outcome` event for a
/// line that ended a pane's wait (not a timeout's).
pub(crate) fn finish_wait_line(
    line: &mut serde_json::Value,
    results: &HashMap<u64, PathBuf>,
    since: SystemTime,
    resumed: &HashMap<u64, SystemTime>,
) -> Option<serde_json::Value> {
    let pane = line_pane(line)?;
    let since = resumed.get(&pane).copied().unwrap_or(since);
    if let Some(info) = results
        .get(&pane)
        .and_then(|path| result::written_since(path, since))
    {
        line["result"] = info;
    }
    let reason = line.get("reason")?;
    Some(serde_json::json!({
        "pane": pane,
        "reason": reason,
        "seq": line.get("seq"),
        "label": line.get("label"),
    }))
}

/// A wait resumed with `--after PANE=SEQ` (as `send --wait`'s resume hint
/// says): for each such pane, when the journal's `send` with that
/// `seqBefore` was made (its `ts`, whole seconds rounded down). A report
/// written between that send and this wait still counts as its result.
pub(crate) fn resumed_since(
    events: &[serde_json::Value],
    after: &HashMap<u64, u64>,
) -> HashMap<u64, SystemTime> {
    after
        .iter()
        .filter_map(|(&pane, &seq)| {
            let ts = journal::sent_at(events, pane, seq)?;
            Some((pane, SystemTime::UNIX_EPOCH + Duration::from_secs(ts)))
        })
        .collect()
}

/// `status`: a line per worker in `children`, with what the journal knows
/// of it (`workers`), and for a worktree worker `ahead`/`dirty` from `git`
/// (given its branch and worktree); then the journal's workers that are no
/// longer listed, as `"gone":true`.
pub(crate) fn status_lines(
    children: &[Panel],
    workers: &[journal::Worker],
    mut git: impl FnMut(&str, &str) -> (Option<u64>, Option<bool>),
) -> Vec<serde_json::Value> {
    let known = |pane: u64| workers.iter().find(|worker| worker.pane == pane);
    let mut lines = Vec::new();
    for panel in children {
        let pane = panel.pane_id.0;
        let mut line = serde_json::json!({ "paneId": WireId(pane), "cwd": panel.cwd });
        let agent = panel.agent.as_ref();
        let fields = [
            ("label", panel.label.clone().map(Into::into)),
            (
                "state",
                agent.and_then(|agent| agent.state.clone()).map(Into::into),
            ),
            ("seq", agent.map(|agent| agent.seq.into())),
            (
                "blocked",
                agent
                    .and_then(|agent| agent.blocked.clone())
                    .map(Into::into),
            ),
            (
                "exit",
                panel
                    .exit
                    .as_ref()
                    .map(|exit| serde_json::to_value(exit).expect("PanelExit serializes")),
            ),
            ("branch", panel.branch.clone().map(Into::into)),
        ];
        for (key, value) in fields {
            if let Some(value) = value {
                line[key] = value;
            }
        }
        let worker = known(pane);
        if let Some(worker) = worker {
            worker.add_to(&mut line);
        }
        if line.get("argv").is_none()
            && let Some(spawn) = &panel.spawn
        {
            line["argv"] = spawn.argv.clone().into();
        }
        // A `spawn --worktree` worker: its branch and worktree from the
        // journal, else an `orch/` branch checked out in its cwd.
        let checkout = match worker
            .and_then(|worker| Some((worker.branch.clone()?, worker.worktree.clone()?)))
        {
            Some(checkout) => Some(checkout),
            None => panel
                .branch
                .clone()
                .filter(|branch| branch.starts_with("orch/"))
                .map(|branch| (branch, panel.cwd.clone())),
        };
        if let Some((branch, dir)) = checkout {
            let (ahead, dirty) = git(&branch, &dir);
            if let Some(ahead) = ahead {
                line["ahead"] = ahead.into();
            }
            if let Some(dirty) = dirty {
                line["dirty"] = dirty.into();
            }
        }
        lines.push(line);
    }
    for worker in workers {
        if children.iter().any(|panel| panel.pane_id.0 == worker.pane) {
            continue;
        }
        let mut line = serde_json::json!({ "paneId": WireId(worker.pane), "gone": true });
        if let Some(label) = &worker.label {
            line["label"] = label.clone().into();
        }
        if worker.closed {
            line["closed"] = true.into();
        }
        if let Some(cwd) = &worker.cwd {
            line["cwd"] = cwd.clone().into();
        }
        worker.add_to(&mut line);
        lines.push(line);
    }
    lines
}

/// How many screen lines a prompt, a spawn's first screen or an answered
/// prompt keeps.
pub(crate) const PROMPT_LINES: usize = 20;

/// The last `lines` non-blank lines of `text`.
pub(crate) fn last_lines(text: &str, lines: usize) -> String {
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    kept[kept.len().saturating_sub(lines)..].join("\n")
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

/// The stderr line for a retained-output read (`read --scrollback`, `wait
/// --tail`) of a pane on the alternate screen, which keeps no history: only
/// its screen came back (REL-07). A server that predates `alternateScreen`
/// never says so.
pub(crate) fn history_notice(
    pane: u64,
    source: TextSource,
    result: &noa_ipc::GetTextResult,
) -> Option<String> {
    (source == TextSource::Scrollback && result.alternate_screen).then(|| {
        format!(
            "Noa +ctl: pane {pane} is on the alternate screen; its program keeps no history \
             here: only the screen is returned"
        )
    })
}

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
/// `emit`; returns the exit status. `prune` is asked about each closed pane
/// with a label: a listed one's label and cwd, or for one that is not
/// listed (already gone), its label and worktree from the journal
/// (`workers`).
pub(crate) fn close_panes(
    source: &mut impl PaneCloser,
    panes: &[u64],
    own: Option<u64>,
    workers: &[journal::Worker],
    prune: &mut dyn FnMut(u64, &str, &str) -> Option<serde_json::Value>,
    emit: &mut dyn FnMut(&serde_json::Value),
) -> Result<i32, Failure> {
    let panels = source.list()?;
    let mut failed = false;
    let mut unconfirmed = false;
    for (pane, listed) in close_targets(&panels, panes, own).map_err(Failure::error)? {
        let label = match panels.iter().find(|panel| panel.pane_id.0 == pane) {
            Some(panel) => panel.label.clone().map(|label| (label, panel.cwd.clone())),
            None => workers
                .iter()
                .find(|worker| worker.pane == pane)
                .and_then(|worker| Some((worker.label.clone()?, worker.worktree.clone()?))),
        };
        let gone = !listed
            || match source.close(pane) {
                Ok(()) => false,
                // Refused: fine if the pane went away since the listing. A
                // missing scope refuses every pane, so it fails the command.
                Err(ClientError::Rpc { code, message }) if code != RPC_SCOPE_DENIED => {
                    let listed = source.list()?;
                    let still_listed = listed.iter().any(|panel| panel.pane_id.0 == pane);
                    // Not confirmed in time: the close may still happen.
                    if code == RPC_OUTCOME_UNKNOWN {
                        if still_listed {
                            eprintln!(
                                "Noa +ctl: Noa did not confirm closing pane {pane} in time \
                                 ({message}); it may still close: check with `ls` before \
                                 retrying"
                            );
                            unconfirmed = true;
                            continue;
                        }
                        false
                    } else if still_listed {
                        eprintln!("Noa +ctl: pane {pane} was not closed: {message}");
                        failed = true;
                        continue;
                    } else {
                        true
                    }
                }
                Err(err) => return Err(err.into()),
            };
        let mut line = serde_json::json!({ "paneId": WireId(pane), "closed": true });
        if gone {
            line["gone"] = true.into();
        }
        // Only after the pane is closed (or went away since the listing, its
        // label and cwd already read), so its process no longer holds the
        // worktree; a refusal keeps it and does not fail.
        if let Some((label, pane_cwd)) = label
            && let Some(info) = prune(pane, &label, &pane_cwd)
        {
            line["worktree"] = info;
        }
        emit(&line);
    }
    Ok(if unconfirmed {
        EXIT_UNAVAILABLE
    } else if failed {
        EXIT_ERROR
    } else {
        EXIT_OK
    })
}

/// The line `spawn` prints: the new pane, and whatever is known of where and
/// how it started (`worktree` is the worktree's path and branch).
fn spawn_line(
    pane: u64,
    recovered: bool,
    worktree: Option<(&std::path::Path, &str)>,
    tier: Option<&str>,
    result: Option<&str>,
    cwd: Option<&str>,
) -> serde_json::Value {
    let mut line = serde_json::json!({ "paneId": WireId(pane) });
    if recovered {
        line["recovered"] = true.into();
    }
    if let Some((path, branch)) = worktree {
        line["worktree"] = path.to_string_lossy().into_owned().into();
        line["branch"] = branch.into();
    }
    if let Some(tier) = tier {
        line["tier"] = tier.into();
    }
    if let Some(path) = result {
        line["result"] = path.into();
    }
    if let Some(dir) = cwd {
        line["cwd"] = dir.into();
    }
    line
}

/// `--cwd` joined onto this process's cwd (a relative one is taken from it,
/// since Noa resolves the path in its own process), or this process's cwd;
/// not normalized, so it is the path as the caller gave it.
fn joined_cwd(
    requested: Option<String>,
    current: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    match (requested, current) {
        (Some(dir), Some(current)) => Some(current.join(dir)),
        (Some(dir), None) => Some(std::path::PathBuf::from(dir)),
        (None, current) => current,
    }
}

/// The worker's cwd: [`joined_cwd`], normalized by [`remove_dot_segments`].
/// An absolute result has no `.` or `..` component, except a path with a
/// `..` whose directory does not exist: that one is returned unchanged (for
/// `check_spawn` to refuse). A path with no such segment comes out
/// byte-for-byte unchanged.
pub(crate) fn spawn_cwd(
    requested: Option<String>,
    current: Option<std::path::PathBuf>,
) -> Option<String> {
    remove_dot_segments(joined_cwd(requested, current)?)
        .into_os_string()
        .into_string()
        .ok()
}

/// `path` without `.` and `..` components, naming the same directory.
///
/// No such segment: unchanged, with no filesystem access. Only `.`: they are
/// dropped. A `..`: popped lexically, unless the directory does not exist
/// (returned unchanged, so the caller refuses it) or a symlink before the `..`
/// makes the kernel's answer differ (then the canonical path). A relative
/// path is returned unchanged.
fn remove_dot_segments(path: std::path::PathBuf) -> std::path::PathBuf {
    use std::path::Component;

    if !path.is_absolute() {
        return path;
    }
    let text = path.to_string_lossy();
    let has = |dot: &str| text.split('/').any(|segment| segment == dot);
    let (has_dot, has_parent) = (has("."), has(".."));
    if !has_dot && !has_parent {
        return path;
    }
    let mut lexical = std::path::PathBuf::new();
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if depth > 0 {
                    lexical.pop();
                    depth -= 1;
                }
            }
            Component::Normal(name) => {
                lexical.push(name);
                depth += 1;
            }
            other => lexical.push(other.as_os_str()),
        }
    }
    if !has_parent {
        return lexical;
    }
    match path.canonicalize() {
        Err(_) => path,
        Ok(real) if lexical.canonicalize().is_ok_and(|l| l == real) => lexical,
        Ok(real) => real,
    }
}

fn print_panel(mut panel: Panel) {
    // The preview is for UIs; a shell reads output with `read`.
    panel.preview.clear();
    print_json(&serde_json::to_value(panel).expect("Panel serializes"));
}

fn print_json(value: &serde_json::Value) {
    write_json_line(&mut std::io::stdout().lock(), value);
}

/// One JSON line on `out`. A reader that went away (`close --children |
/// head -1`) is not a reason to stop: the command still does all its work.
pub(crate) fn write_json_line(out: &mut impl Write, value: &serde_json::Value) {
    if let Err(err) = writeln!(out, "{value}")
        && err.kind() != std::io::ErrorKind::BrokenPipe
    {
        panic!("failed printing to stdout: {err}");
    }
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
                blocked: None,
            }),
            exit: None,
            parent_pane_id: Some(WireId(3)),
            label: None,
            spawn: None,
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
                ready: None,
                task: None,
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

    // Audit finding (AU-02): a send right after a spawn raced the worker's
    // start; `--ready` waits for it, and `--task` types once it is ready.
    #[test]
    fn spawn_ready_and_task_parse_with_the_tasks_wait_options() {
        let spawn = |items: &[&str]| match parse_args(&args(items)) {
            Ok(Command::Spawn(spawn)) => Ok(spawn),
            Ok(other) => panic!("spawn expected, got {other:?}"),
            Err(error) => Err(error),
        };
        let ready = spawn(&["spawn", "--ready", "20", "--", "make"]).unwrap();
        assert_eq!(ready.ready, Some(Duration::from_secs(20)));
        assert_eq!(ready.task, None);

        let task = spawn(&[
            "spawn",
            "--task",
            "fix it",
            "--timeout",
            "500",
            "--settle",
            "10",
            "--",
            "claude",
        ])
        .unwrap();
        assert_eq!(task.ready, None);
        let given = task.task.unwrap();
        assert_eq!(given.text, TaskText::Literal("fix it".to_string()));
        assert_eq!(given.wait.timeout, Some(Duration::from_secs(500)));
        assert_eq!(given.wait.settle, Some(Duration::from_secs(10)));
        let file = spawn(&[
            "spawn",
            "--task-file",
            "t.md",
            "--ready",
            "5",
            "--",
            "codex",
        ])
        .unwrap();
        assert_eq!(file.task.unwrap().text, TaskText::File("t.md".to_string()));
        assert_eq!(file.ready, Some(Duration::from_secs(5)));

        for bad in [
            &["spawn", "--task", "x", "--", "make"][..],
            &["spawn", "--task", "x"],
            &["spawn", "--task", "x", "--task-file", "f", "--", "claude"],
            &["spawn", "--ready", "soon", "--", "claude"],
            // Wait options go with --task only.
            &["spawn", "--timeout", "5", "--", "claude"],
        ] {
            assert!(spawn(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    // Audit finding (REL-14): a worker labelled `board:7` was taken for
    // pane 7's board.
    #[test]
    fn spawn_refuses_a_board_label() {
        for bad in [
            &["spawn", "--label", "board:7"][..],
            &["spawn", "--label=board:", "--", "zsh"],
        ] {
            let err = parse_args(&args(bad)).unwrap_err();
            assert!(err.contains("board:"), "{bad:?}: {err}");
        }
        assert!(parse_args(&args(&["spawn", "--label", "my-board:7"])).is_ok());
    }

    // Audit finding (AU-14): `--input` is raw keys written as the worker
    // starts, so a task given to an agent that way had each newline taken
    // as Enter.
    #[test]
    fn spawn_refuses_input_for_an_agent_command() {
        for agent in ["claude", "codex", "/usr/local/bin/agy"] {
            let err = parse_args(&args(&["spawn", "--input", "task", "--", agent])).unwrap_err();
            assert!(err.contains("send"), "{agent}: {err}");
        }
        assert!(parse_args(&args(&["spawn", "--input", "make test", "--", "zsh"])).is_ok());
        assert!(parse_args(&args(&["spawn", "--input", "claude"])).is_ok());
    }

    /// What `run_spawn_steps` returns: the result and the steps taken.
    type StepsRun = (Result<(u64, Option<&'static str>), Failure>, Vec<String>);

    /// `spawn_steps` with each step logged; `worktree` fails when
    /// `worktree_fails`, `spawn` fails with `spawn_error`.
    fn run_spawn_steps(worktree_fails: bool, spawn_error: Option<ClientError>) -> StepsRun {
        let (result, log) = run_spawn_steps_with(worktree_fails, spawn_error, false, None);
        (result.map(|spawned| (spawned.pane, spawned.made)), log)
    }

    /// As [`run_spawn_steps`]; `host` says it opened the board when
    /// `opened_board`, and `recover` finds `recovered`.
    fn run_spawn_steps_with(
        worktree_fails: bool,
        spawn_error: Option<ClientError>,
        opened_board: bool,
        recovered: Option<u64>,
    ) -> (Result<Spawned<&'static str>, Failure>, Vec<String>) {
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
                Host {
                    pane: 7,
                    opened_board,
                }
            },
            |log, host, made| {
                log.push(format!("spawn on {host} in {made:?}"));
                match spawn_error {
                    Some(err) => Err(err),
                    None => Ok(9),
                }
            },
            |log| {
                log.push("recover".to_string());
                recovered
            },
            |log, made, refused| log.push(format!("undo {made} refused={refused}")),
            |log, board| log.push(format!("close board {board}")),
        );
        (result, log)
    }

    fn outcome_unknown() -> ClientError {
        ClientError::Rpc {
            code: RPC_OUTCOME_UNKNOWN,
            message: "outcome unknown: request timed out".to_string(),
        }
    }

    // Audit finding (AU-06): a spawn the app did not confirm in time was
    // exit 1, read as a refusal, and a second worker was started.
    #[test]
    fn a_spawn_whose_answer_was_lost_finds_its_worker() {
        for error in [
            outcome_unknown(),
            ClientError::Timeout("noa.split".to_string()),
        ] {
            let (result, log) = run_spawn_steps_with(false, Some(error), true, Some(12));
            assert_eq!(
                result.ok(),
                Some(Spawned {
                    pane: 12,
                    made: Some("wt"),
                    recovered: true,
                })
            );
            // Its worktree is the recovered worker's, and so is the board.
            assert_eq!(log.last().map(String::as_str), Some("recover"));
        }

        // Not found: exit 3, and the worktree and board are kept.
        let (result, log) = run_spawn_steps_with(false, Some(outcome_unknown()), true, None);
        assert_eq!(result.err().map(|f| f.code), Some(EXIT_UNAVAILABLE));
        assert_eq!(log[log.len() - 2..], ["recover", "undo wt refused=false"]);

        // A refusal or a lost connection is not looked for.
        let refusal = ClientError::Rpc {
            code: -32602,
            message: "nesting goes one level deep".to_string(),
        };
        let (_, log) = run_spawn_steps_with(false, Some(refusal), false, Some(12));
        assert!(!log.contains(&"recover".to_string()));
        let lost = ClientError::Transport("connection reset".to_string());
        let (_, log) = run_spawn_steps_with(false, Some(lost), false, Some(12));
        assert!(!log.contains(&"recover".to_string()));
    }

    #[test]
    fn a_recovered_worker_is_the_one_new_child_with_the_label() {
        let mut new = worker(12, 1);
        new.label = Some("api".to_string());
        let panels = vec![worker(8, 1), new.clone(), worker(13, 2)];
        assert_eq!(recovered_child(&[8], &panels, 1, None), Some(12));
        assert_eq!(recovered_child(&[8], &panels, 1, Some("api")), Some(12));
        assert_eq!(recovered_child(&[8], &panels, 1, Some("web")), None);
        assert_eq!(recovered_child(&[8, 12], &panels, 1, None), None);
        // Two new children: which one is this spawn's is not known.
        let two = vec![worker(8, 1), new, worker(14, 1)];
        assert_eq!(recovered_child(&[8], &two, 1, None), None);
        assert_eq!(recovered_child(&[8], &two, 1, Some("api")), Some(12));
    }

    // Audit finding (REL-13): a spawn the server refused left the board it
    // had just opened, empty, next to the orchestrator.
    #[test]
    fn a_refused_spawn_closes_the_board_it_opened() {
        let refusal = || ClientError::Rpc {
            code: -32000,
            message: "the float could not open".to_string(),
        };
        let (result, log) = run_spawn_steps_with(false, Some(refusal()), true, None);
        assert_eq!(result.err().map(|f| f.code), Some(EXIT_ERROR));
        assert_eq!(log.last().map(String::as_str), Some("close board 7"));

        // A board that was already open, or an outcome not known, stays.
        let (_, log) = run_spawn_steps_with(false, Some(refusal()), false, None);
        assert!(log.iter().all(|entry| !entry.starts_with("close board")));
        let (_, log) = run_spawn_steps_with(false, Some(outcome_unknown()), true, None);
        assert!(log.iter().all(|entry| !entry.starts_with("close board")));
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
            &[],
            &mut |_pane, label, _cwd| {
                asked.push(label.to_string());
                Some(serde_json::json!({ "path": "/r-wt/job", "removed": true }))
            },
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
        // No label (9) and already gone, unknown to the journal (7), are not
        // asked about.
        assert_eq!(asked, ["job"]);
        assert_eq!(lines[0]["worktree"]["removed"], true);
        assert!(lines[1].get("worktree").is_none());
        assert_eq!(lines[2]["gone"], true);
        assert!(lines[2].get("worktree").is_none());
    }

    // Review finding #7: `close --prune` on a worktree worker that already
    // vanished printed only "gone": its worktree and branch stayed, and
    // nothing said so. The journal knows its label and worktree.
    #[test]
    fn prune_finds_a_gone_workers_worktree_from_the_journal() {
        let mut source = OkCloser(vec![worker(9, 1)]);
        let workers = [
            journal::Worker {
                pane: 7,
                label: Some("fix".to_string()),
                cwd: Some("/r-wt/fix/src".to_string()),
                worktree: Some("/r-wt/fix".to_string()),
                ..journal::Worker::default()
            },
            // Known, but spawned without --worktree: nothing to prune.
            journal::Worker {
                pane: 6,
                label: Some("plain".to_string()),
                cwd: Some("/r".to_string()),
                ..journal::Worker::default()
            },
        ];
        let mut asked = Vec::new();
        let mut lines = Vec::new();
        let code = close_panes(
            &mut source,
            &[7, 6],
            Some(1),
            &workers,
            &mut |pane, label, cwd| {
                asked.push((pane, label.to_string(), cwd.to_string()));
                Some(serde_json::json!({ "path": cwd, "removed": true }))
            },
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(asked, [(7, "fix".to_string(), "/r-wt/fix".to_string())]);
        assert_eq!(lines[0]["gone"], true);
        assert_eq!(lines[0]["worktree"]["path"], "/r-wt/fix");
        assert_eq!(lines[1]["gone"], true);
        assert!(lines[1].get("worktree").is_none());
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
                queue: false,
                new_run: false,
                wait: Some(WaitOptions {
                    reasons: vec![Reason::Finished, Reason::Error],
                    all: false,
                    timeout: Some(Duration::from_secs(30)),
                    tail: None,
                    stall: None,
                    queued: false,
                    settle: None,
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
                any_pane: false,
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
                closed: false,
                agent_left: false
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
            outcome_json(&watch, &outcome, Some(&held), None)["exitCode"],
            serde_json::json!(2)
        );
        // A pane that disappeared ends the wait whatever was asked for.
        assert_eq!(
            evaluate(&mut watch, None, &[Reason::Finished]),
            Some(Outcome {
                reason: Reason::Exit,
                closed: true,
                agent_left: false
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

    // board-timeline AC-3: `board --timeline` runs the board here in its
    // timeline view, of this pane's board or of the one `--board-id` names.
    #[test]
    fn board_timeline_runs_here_on_this_or_a_named_board() {
        assert_eq!(
            parse_args(&args(&["board", "--timeline"])),
            Ok(Command::BoardTimeline {
                of: None,
                board_id: None
            })
        );
        assert_eq!(
            parse_args(&args(&[
                "board",
                "--timeline",
                "--board-id",
                "bd-0123456789abcdef",
                "--of",
                "self"
            ])),
            Ok(Command::BoardTimeline {
                of: Some(BoardOf::Own),
                board_id: Some("bd-0123456789abcdef".to_string())
            })
        );
        assert!(parse_args(&args(&["board", "--board-id", "bd-0123456789abcdef"])).is_err());
        assert!(parse_args(&args(&["board", "--timeline", "--board-id", "../x"])).is_err());
        assert!(parse_args(&args(&["board", "--timeline", "--open"])).is_err());
    }

    // board-timeline AC-4: only `send --new-run` starts a new run; an answer
    // or raw keys never do.
    #[test]
    fn send_new_run_is_a_flag_refused_with_answer_or_raw() {
        assert!(send_args(&["--new-run", "next task"]).new_run);
        assert!(!send_args(&["next task"]).new_run);
        assert!(parse_args(&args(&["send", "8", "--new-run", "--answer", "1"])).is_err());
        assert!(parse_args(&args(&["send", "8", "--new-run", "--raw", "x"])).is_err());
    }

    // Review defect 4: `--queue` and `--replace` continue the same run
    // (AC-4), so `--new-run` with either is refused before anything runs.
    #[test]
    fn send_new_run_is_refused_with_queue_or_replace() {
        for flag in ["--queue", "--replace"] {
            let message =
                parse_args(&args(&["send", "8", "--new-run", flag, "next task"])).unwrap_err();
            assert!(message.contains("--new-run"), "{flag}: {message}");
        }
        assert!(!send_args(&["--queue", "more"]).new_run);
        assert!(!send_args(&["--replace", "more"]).new_run);
    }

    #[test]
    fn ac4_spawn_task_checks_the_encoded_payload_before_spawning() {
        let header = run_attr::with_longest_header("");
        let overhead = serde_json::to_string(&header).unwrap().len();
        let body = "x".repeat(MAX_SEND_TEXT_BYTES - overhead);
        let prepare = |body: &str, supports_timeline| {
            Task {
                text: TaskText::Literal(body.to_owned()),
                wait: WaitOptions::default(),
            }
            .prepare(supports_timeline)
        };
        assert!(prepare(&body, true).is_ok());
        let over = body + "x";
        assert!(prepare(&over, true).unwrap_err().message.contains("KiB"));
        assert_eq!(prepare(&over, false).unwrap().0, over);
    }

    // Review defect 2: every refusal `send` knows before typing is checked
    // before a run is registered, so a refused `send --new-run` registers
    // (and journals) nothing.
    #[test]
    fn send_new_run_registers_only_once_the_send_would_go() {
        let attr = run_attr::RunAttr {
            board_id: "bd-0123456789abcdef".to_string(),
            run_id: "rn-0123456789abcdef".to_string(),
            worker_id: Some("wk-0123456789abcdef".to_string()),
            producer_id: "wk-0123456789abcdef".to_string(),
            retry_of: None,
        };
        let send = send_args(&["--new-run", "--enter", "next task"]);
        let at_limit = "x".repeat(MAX_SEND_TEXT_BYTES - 2);
        assert!(check_send_size(&at_limit).is_ok());
        for (panel, text, why) in [
            (
                blocked(2, "permission", "permission"),
                "next task",
                "an approval",
            ),
            (
                blocked(2, "input", "question"),
                "next task",
                "an answer to a question",
            ),
            (panel(2, Some("running"), false), "next task", "is working"),
            // Fits alone, not with the header in front.
            (panel(2, Some("finished"), false), at_limit.as_str(), "KiB"),
        ] {
            let mut target = FakeTarget::new(vec![panel]);
            let mut registered = 0;
            let failure = new_run_text(&mut target, &send, text, true, |_| {
                registered += 1;
                Some(attr.clone())
            })
            .unwrap_err();
            assert!(failure.message.contains(why), "{why}: {}", failure.message);
            assert_eq!(registered, 0, "{why}");
            assert!(target.log.is_empty(), "{why}: {:?}", target.log);
        }
        // Bracketed paste that never comes on refuses too.
        let mut target = FakeTarget::new(vec![panel(2, Some("finished"), false)]);
        target.paste_ready_after = Some(usize::MAX);
        let mut registered = 0;
        assert!(
            new_run_text(&mut target, &send, "next task", true, |_| {
                registered += 1;
                None
            })
            .is_err()
        );
        assert_eq!(registered, 0);

        // A send that goes registers once, then types the header first.
        let mut target = FakeTarget::new(vec![panel(2, Some("finished"), false)]);
        let mut registered = 0;
        let text = new_run_text(&mut target, &send, "next task", true, |_| {
            registered += 1;
            Some(attr.clone())
        })
        .unwrap();
        assert_eq!(registered, 1);
        assert_eq!(text, run_attr::with_header(Some(&attr), "next task"));
        assert!(
            target.log.iter().all(|line| line == "paste ready?"),
            "{:?}",
            target.log
        );
        // No timeline: the text goes as it is.
        let mut target = FakeTarget::new(vec![panel(2, Some("finished"), false)]);
        assert_eq!(
            new_run_text(&mut target, &send, "next task", false, |_| None).unwrap(),
            "next task"
        );
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
                of: Some(BoardOf::Pane(3)),
                open: None
            })
        );
        assert_eq!(
            parse_args(&args(&["board", "--of", "self"])),
            Ok(Command::Board {
                of: Some(BoardOf::Own),
                open: None
            })
        );
        assert!(parse_args(&args(&["board", "--open", "--of", "self"])).is_err());
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
                    removed_pane_ids: Vec::new(),
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
                    removed_pane_ids: Vec::new(),
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
                    removed_pane_ids: Vec::new(),
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
                closed: false,
                agent_left: false
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
                removed_pane_ids: Vec::new(),
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
        let failure = close_panes(
            &mut source,
            &[8],
            Some(1),
            &[],
            &mut |_, _, _| None,
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(failure.code, EXIT_UNAVAILABLE);

        // Any other refusal of a pane that is still listed fails that pane only.
        source.refuse_code = -32000;
        let mut lines = Vec::new();
        let code = close_panes(
            &mut source,
            &[8],
            Some(1),
            &[],
            &mut |_, _, _| None,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_ERROR));
        assert!(lines.is_empty());
    }

    // Audit finding (AU-06): a close the app did not confirm in time was a
    // failed pane (exit 1), though it may still close.
    #[test]
    fn an_unconfirmed_close_exits_3_and_does_not_stop_the_rest() {
        let mut source = FakeCloser {
            panels: vec![worker(8, 1)],
            refuse_code: RPC_OUTCOME_UNKNOWN,
        };
        let mut lines = Vec::new();
        let code = close_panes(
            &mut source,
            &[8, 9],
            Some(1),
            &[],
            &mut |_, _, _| None,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_UNAVAILABLE));
        // 9 was already gone and is still reported.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["paneId"], "9");

        // Gone by the time it is looked at again: it closed.
        let mut source = VanishingCloser {
            panels: vec![worker(8, 1)],
            lists: 0,
            code: RPC_OUTCOME_UNKNOWN,
        };
        let mut lines = Vec::new();
        let code = close_panes(
            &mut source,
            &[8],
            Some(1),
            &[],
            &mut |_, _, _| None,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines, [serde_json::json!({"paneId": "8", "closed": true})]);
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

    fn blocked(seq: u64, state: &str, on: &str) -> Panel {
        let mut panel = panel(seq, Some(state), false);
        panel.agent.as_mut().unwrap().blocked = Some(on.to_string());
        panel
    }

    // Audit finding (AU-01): an `OSC 7501` question shows as `input`, and
    // `send --enter` picked its first choice. Only a reported block is
    // refused; a plain `input` (hook adapter, older server) still takes it.
    #[test]
    fn a_blocked_agent_takes_a_send_only_with_answer() {
        assert_eq!(
            awaits_answer(&blocked(2, "input", "question")),
            Some("an answer to a question")
        );
        assert_eq!(
            awaits_answer(&blocked(2, "permission", "permission")),
            Some("an approval")
        );
        let mut target = FakeTarget::new(vec![blocked(2, "input", "question")]);
        let failure = send_steps(&mut target, &send_args(&["--enter", "go"]), "go").unwrap_err();
        assert_eq!(failure.code, EXIT_ERROR);
        assert!(
            failure.message.starts_with(
                "pane 8 is waiting for an answer to a question; read it and pass --answer"
            ),
            "{}",
            failure.message
        );
        assert!(target.log.is_empty());

        let mut target = FakeTarget::new(vec![blocked(2, "input", "question")]);
        assert!(send_steps(&mut target, &send_args(&["--answer", "--enter", "2"]), "2").is_ok());
        let mut target = FakeTarget::new(vec![panel(2, Some("input"), false)]);
        assert!(send_steps(&mut target, &send_args(&["--enter", "go"]), "go").is_ok());

        // The wait line names what the agent is blocked on.
        let watch = Watch::new(8, None, None);
        let outcome = Outcome {
            reason: Reason::Input,
            closed: false,
            agent_left: false,
        };
        let line = outcome_json(
            &watch,
            &outcome,
            Some(&blocked(2, "input", "question")),
            None,
        );
        assert_eq!(line["blocked"], "question");
        let line = outcome_json(
            &watch,
            &outcome,
            Some(&panel(2, Some("input"), false)),
            None,
        );
        assert!(line.get("blocked").is_none());
    }

    /// A scripted pane for [`send_steps`]: `panels` are answered in order
    /// (the last one repeats); every write is logged.
    struct FakeTarget {
        panels: Vec<Panel>,
        looks: usize,
        log: Vec<String>,
        /// Bracketed paste is on from this many asks on (`None`: always).
        paste_ready_after: Option<usize>,
        paste_asks: usize,
        paused: Duration,
    }

    impl FakeTarget {
        fn new(panels: Vec<Panel>) -> Self {
            Self {
                panels,
                looks: 0,
                log: Vec::new(),
                paste_ready_after: None,
                paste_asks: 0,
                paused: Duration::ZERO,
            }
        }
    }

    impl SendTarget for FakeTarget {
        fn panel(&mut self, _pane: u64) -> Result<Option<Panel>, Failure> {
            let index = self.looks.min(self.panels.len() - 1);
            self.looks += 1;
            Ok(Some(self.panels[index].clone()))
        }

        fn subscribe(&mut self, _pane: u64) -> Result<(), Failure> {
            self.log.push("subscribe".to_string());
            Ok(())
        }

        fn type_text(&mut self, _pane: u64, text: &str, paste: bool) -> Result<(), Failure> {
            self.log.push(format!("type {text:?} paste={paste}"));
            Ok(())
        }

        fn paste_ready(&mut self, _pane: u64) -> Result<bool, Failure> {
            self.log.push("paste ready?".to_string());
            let ready = self
                .paste_ready_after
                .is_none_or(|after| self.paste_asks >= after);
            self.paste_asks += 1;
            Ok(ready)
        }

        fn pause(&mut self, duration: Duration) {
            self.paused += duration;
        }
    }

    fn send_args(items: &[&str]) -> SendArgs {
        let mut full = vec!["send", "8"];
        full.extend(items);
        match parse_args(&args(&full)).unwrap() {
            Command::Send(send) => send,
            other => panic!("send expected, got {other:?}"),
        }
    }

    // Audit finding (REL-04): a text over the server's 256 KiB frame limit
    // dropped the connection, read as Noa gone (exit 3), after --replace
    // had already cleared the input line.
    #[test]
    fn a_text_too_large_for_one_request_is_refused_before_anything_is_typed() {
        let fits = "x".repeat(MAX_SEND_TEXT_BYTES - 2);
        assert!(check_send_size(&fits).is_ok());
        // Each newline doubles under JSON encoding.
        let over = "\n".repeat(MAX_SEND_TEXT_BYTES / 2);
        let message = check_send_size(&over).unwrap_err();
        assert!(message.contains("file"), "{message}");

        let mut target = FakeTarget::new(vec![panel(1, Some("finished"), false)]);
        let failure = send_steps(&mut target, &send_args(&["--replace", "x"]), &over).unwrap_err();
        assert_eq!(failure.code, EXIT_ERROR);
        assert!(target.log.is_empty(), "{:?}", target.log);
    }

    // Audit finding (AU-05): a send to a working agent was queued by it, and
    // `--wait` then ended with the turn already running.
    #[test]
    fn a_working_agent_takes_a_send_only_with_queue() {
        let running = panel(3, Some("running"), false);
        let mut target = FakeTarget::new(vec![running.clone()]);
        let failure =
            send_steps(&mut target, &send_args(&["--enter", "more"]), "more").unwrap_err();
        assert_eq!(failure.code, EXIT_ERROR);
        assert!(failure.message.contains("--queue"), "{}", failure.message);
        assert!(target.log.is_empty());

        for flags in [&["--queue", "--enter", "more"][..], &["--answer", "more"]] {
            let mut target = FakeTarget::new(vec![running.clone()]);
            assert!(
                send_steps(&mut target, &send_args(flags), "more").is_ok(),
                "{flags:?}"
            );
        }
        // Not working: no flag needed.
        let mut target = FakeTarget::new(vec![panel(3, Some("finished"), false)]);
        assert!(send_steps(&mut target, &send_args(&["more"]), "more").is_ok());

        // Raw keys interrupt a working agent (Esc, Ctrl-C), so they go; a
        // bare Enter is refused like pasted text.
        let mut target = FakeTarget::new(vec![running.clone()]);
        assert!(send_steps(&mut target, &send_args(&["--raw", "\x1b"]), "\x1b").is_ok());
        assert_eq!(target.log, vec!["type \"\\u{1b}\" paste=false".to_string()]);
        for (flags, text) in [(&["--enter", ""][..], ""), (&["--raw", "--enter", ""], "")] {
            let mut target = FakeTarget::new(vec![running.clone()]);
            let failure = send_steps(&mut target, &send_args(flags), text).unwrap_err();
            assert!(failure.message.contains("--queue"), "{flags:?}");
            assert!(target.log.is_empty(), "{flags:?}");
        }

        // `--queue --wait` marks every wait line.
        let send = send_args(&["--queue", "--wait", "--timeout", "0", "more"]);
        let options = send.wait.unwrap();
        assert!(options.queued);
        let mut source = FakeSource {
            lists: vec![vec![with_pane(running.clone(), 8)]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &[8],
            send_wait_start(with_pane(running, 8), 3),
            &options,
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_TIMEOUT));
        assert_eq!(lines[0]["queued"], true);
        assert!(!send_args(&["--wait", "x"]).wait.unwrap().queued);
    }

    // Audit finding (REL-01): a send right after a spawn reached an agent
    // that had not enabled bracketed paste yet, so each newline of the text
    // landed as Enter.
    #[test]
    fn a_paste_into_an_agent_waits_for_bracketed_paste_before_any_key() {
        let mut target = FakeTarget::new(vec![panel(0, None, false)]);
        target.paste_ready_after = Some(3);
        send_steps(
            &mut target,
            &send_args(&["--replace", "--enter", "a\nb"]),
            "a\nb",
        )
        .unwrap();
        assert_eq!(target.paste_asks, 4);
        assert_eq!(
            target.paused,
            PASTE_MODE_PAUSE * 3 + REPLACE_PAUSE * 5 + SUBMIT_DELAY * 2
        );
        let first_key = target
            .log
            .iter()
            .position(|entry| entry.starts_with("type"))
            .unwrap();
        assert!(
            target.log[..first_key]
                .iter()
                .all(|entry| entry == "paste ready?")
        );
        assert_eq!(first_key, 4);

        // Never ready: nothing at all is typed after 15 s.
        let mut target = FakeTarget::new(vec![panel(0, None, false)]);
        target.paste_ready_after = Some(usize::MAX);
        let failure = send_steps(
            &mut target,
            &send_args(&["--replace", "--enter", "go"]),
            "go",
        )
        .unwrap_err();
        assert_eq!(failure.code, EXIT_ERROR);
        assert_eq!(
            failure.message,
            "pane 8 has not enabled bracketed paste yet (its program may still be starting); \
             nothing was sent"
        );
        assert_eq!(target.paused, Duration::from_secs(15));
        assert!(target.log.iter().all(|entry| !entry.starts_with("type")));

        // Raw keys, a bare Enter and a plain command's pane are not gated.
        let mut shell = panel(0, None, false);
        shell.agent = None;
        shell.process = Some("zsh".to_string());
        for (panel, flags, text) in [
            (panel(0, None, false), &["--raw", "x"][..], "x"),
            (panel(0, None, false), &["--enter", ""], ""),
            (shell.clone(), &["--enter", "make"], "make"),
        ] {
            let mut target = FakeTarget::new(vec![panel]);
            target.paste_ready_after = Some(usize::MAX);
            send_steps(&mut target, &send_args(flags), text).unwrap();
            assert_eq!(target.paste_asks, 0, "{flags:?}");
        }
        // A pane whose program is not listed yet is gated.
        shell.process = None;
        assert!(needs_paste_mode(&shell));
    }

    // Review finding #1: a pane spawned with a non-agent command never gets
    // a process name, so `send` to `-- ./deploy.sh` waited 15 s and was
    // refused. The spawn's argv says it is not starting; an agent's argv
    // still gates through its `kind`.
    #[test]
    fn a_spawned_command_without_a_process_name_is_not_gated() {
        let mut script = panel(0, None, false);
        script.agent = None;
        script.process = None;
        script.spawn = Some(noa_ipc::PanelSpawn {
            argv: vec!["./deploy.sh".to_string()],
            started_at: 0,
        });
        assert!(!needs_paste_mode(&script));
        let mut target = FakeTarget::new(vec![script.clone()]);
        target.paste_ready_after = Some(usize::MAX);
        send_steps(&mut target, &send_args(&["--enter", "y"]), "y").unwrap();
        assert_eq!(target.paste_asks, 0);
        assert!(target.log.iter().any(|entry| entry.starts_with("type")));

        let mut agent = script;
        agent.spawn = Some(noa_ipc::PanelSpawn {
            argv: vec!["claude".to_string()],
            started_at: 0,
        });
        agent.agent = Some(PanelAgent {
            kind: Some("claude".to_string()),
            state: None,
            detail: None,
            seq: 0,
            blocked: None,
        });
        assert!(needs_paste_mode(&agent));
    }

    // Review finding (REL-01): right after `spawn -- claude`, before the
    // pane's first output, the pane is listed bare: no agent, no process,
    // and the spawn's argv. `send` typed into the starting agent without
    // bracketed paste.
    #[test]
    fn a_bare_spawned_agent_without_a_kind_is_still_gated() {
        let mut starting = panel(0, None, false);
        starting.agent = None;
        starting.process = None;
        starting.spawn = Some(noa_ipc::PanelSpawn {
            argv: vec![
                "/opt/homebrew/bin/claude".to_string(),
                "--model".to_string(),
            ],
            started_at: 0,
        });
        assert!(needs_paste_mode(&starting));
        let mut target = FakeTarget::new(vec![starting]);
        target.paste_ready_after = Some(usize::MAX);
        send_steps(&mut target, &send_args(&["--enter", "go"]), "go").unwrap_err();
        assert!(target.paste_asks > 0);
        assert!(target.log.iter().all(|entry| !entry.starts_with("type")));
    }

    // Audit finding (REL-16): `--replace` spends about 0.35 s clearing the
    // input line, and an approval that came up meanwhile took the paste.
    #[test]
    fn replace_checks_for_an_approval_again_before_typing() {
        let mut target = FakeTarget::new(vec![
            panel(3, Some("finished"), false),
            panel(4, Some("permission"), false),
        ]);
        let failure = send_steps(
            &mut target,
            &send_args(&["--replace", "--enter", "new"]),
            "new",
        )
        .unwrap_err();
        assert!(
            failure.message.contains("was not typed"),
            "{}",
            failure.message
        );
        assert!(target.log.iter().all(|entry| !entry.contains("new")));

        let mut target = FakeTarget::new(vec![panel(3, Some("finished"), false)]);
        send_steps(
            &mut target,
            &send_args(&["--replace", "--enter", "new"]),
            "new",
        )
        .unwrap();
        assert_eq!(
            target.log.last().map(String::as_str),
            Some("type \"\\r\" paste=false")
        );
        assert!(target.log.contains(&"type \"new\" paste=true".to_string()));
    }

    // Audit finding (REL-11): a `send --wait` killed before its wait ended
    // left no report count to carry on from.
    #[test]
    fn send_wait_says_how_to_resume() {
        assert_eq!(
            resume_hint(8, 41),
            "Noa +ctl: sent to pane 8; resume with: wait 8 --after 41"
        );
    }

    // Audit finding (AU-06): -32009 (the app did not answer in time) was
    // exit 1, read as a refusal, and the request was sent again.
    #[test]
    fn an_unconfirmed_request_exits_3_and_says_it_may_have_taken_effect() {
        let failure = Failure::from(ClientError::Rpc {
            code: RPC_OUTCOME_UNKNOWN,
            message: "outcome unknown: request timed out".to_string(),
        });
        assert_eq!(failure.code, EXIT_UNAVAILABLE);
        assert!(!failure.transient);
        assert!(
            failure.message.contains("may still have taken effect"),
            "{}",
            failure.message
        );
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

    // Audit findings (REL-10, REL-04): a server at its connection limit, or
    // a request over its frame bound, closed the connection, and both read
    // as "lost the connection to Noa".
    #[test]
    fn a_full_server_is_retried_for_3_s_and_a_large_request_is_an_error() {
        let pauses = connect_retry_pauses();
        assert_eq!(pauses[0], Duration::from_millis(100));
        assert_eq!(pauses[1], Duration::from_millis(200));
        assert_eq!(pauses.iter().sum::<Duration>(), Duration::from_secs(3));

        let full = || ClientError::TooManyConnections("1013 too many connections".to_string());
        let (mut tries, mut slept) = (0, Vec::new());
        let result = connect_retrying(
            || {
                tries += 1;
                if tries < 3 { Err(full()) } else { Ok(tries) }
            },
            |pause| slept.push(pause),
        );
        assert_eq!(result.ok(), Some(3));
        assert_eq!(slept, pauses[..2]);

        let (mut tries, mut slept) = (0, Vec::new());
        let result: Result<(), _> = connect_retrying(
            || {
                tries += 1;
                Err(full())
            },
            |pause| slept.push(pause),
        );
        assert!(matches!(result, Err(ClientError::TooManyConnections(_))));
        assert_eq!(tries, pauses.len() + 1);
        // Review finding #6: a connection dropped during the handshake (a
        // full server past its few 1013 refusals) is retried in the same
        // 3 s, and said as a possibly full Noa once they are spent.
        let dropped = || ClientError::HandshakeDropped("Handshake not finished".to_string());
        let (mut tries, mut slept) = (0, Vec::new());
        let result = connect_retrying(
            || {
                tries += 1;
                match tries {
                    1 => Err(dropped()),
                    2 => Err(full()),
                    _ => Ok(tries),
                }
            },
            |pause| slept.push(pause),
        );
        assert_eq!(result.ok(), Some(3));
        assert_eq!(slept, pauses[..2]);
        let (mut tries, mut slept) = (0, Vec::new());
        let result: Result<(), _> = connect_retrying(
            || {
                tries += 1;
                Err(dropped())
            },
            |pause| slept.push(pause),
        );
        assert!(matches!(result, Err(ClientError::HandshakeDropped(_))));
        assert_eq!(tries, pauses.len() + 1);
        assert_eq!(slept.iter().sum::<Duration>(), Duration::from_secs(3));
        let message = connect_failure_message("ws://127.0.0.1:1/", &dropped());
        assert!(
            message.contains("may be at its connection limit"),
            "{message}"
        );
        // Any other failure is not retried.
        let mut tries = 0;
        let _: Result<(), _> = connect_retrying(
            || {
                tries += 1;
                Err(transport(libc::ECONNREFUSED))
            },
            |_| {},
        );
        assert_eq!(tries, 1);

        let message = connect_failure_message("ws://127.0.0.1:1/", &full());
        assert!(
            message.contains("too many control connections"),
            "{message}"
        );
        let failure = Failure::from(full());
        assert_eq!(failure.code, EXIT_UNAVAILABLE);
        assert!(failure.message.contains("too many control connections"));
        let failure = Failure::from(ClientError::MessageTooBig("1009 frame".to_string()));
        assert_eq!(failure.code, EXIT_ERROR);
        assert!(
            failure.message.contains("size limit"),
            "{}",
            failure.message
        );
    }

    // Audit finding (REL-07): `read --scrollback` of an agent on the
    // alternate screen returned one screen, and nothing said why.
    #[test]
    fn a_history_read_of_the_alternate_screen_says_only_the_screen_came_back() {
        let result = |alternate_screen| noa_ipc::GetTextResult {
            pane_id: WireId(4),
            text: "screen".to_string(),
            truncated: false,
            alternate_screen,
        };
        assert_eq!(
            history_notice(4, TextSource::Scrollback, &result(true)).as_deref(),
            Some(
                "Noa +ctl: pane 4 is on the alternate screen; its program keeps no history \
                 here: only the screen is returned"
            )
        );
        assert_eq!(
            history_notice(4, TextSource::Scrollback, &result(false)),
            None
        );
        assert_eq!(history_notice(4, TextSource::Screen, &result(true)), None);
    }

    // Audit finding (AU-03): a worker's report file was only found by
    // asking; a wait line now names it once written during the wait.
    #[test]
    fn a_wait_line_names_a_result_written_since_the_wait_began() {
        let dir = temp_dir("wait-result");
        let path = result::prepare(&dir, Some("fix")).unwrap();
        let mut worker = with_pane(panel(2, Some("finished"), false), 8);
        worker.cwd = dir.to_string_lossy().into_owned();
        // Unknown to the journal and unlabelled: no file is guessed.
        assert!(result_paths(&[], &[worker.clone()]).is_empty());
        assert!(no_known_result(8).starts_with("pane 8 has no known result file"));
        // Labelled: found by its label.
        worker.label = Some("fix".to_string());
        let results = result_paths(&[], &[worker.clone()]);
        assert_eq!(results[&8], path);
        // The journal's path wins over the cwd's.
        let given = journal::Worker {
            pane: 8,
            result: Some("/elsewhere/r.md".to_string()),
            ..journal::Worker::default()
        };
        assert_eq!(
            result_paths(&[given], &[worker])[&8],
            PathBuf::from("/elsewhere/r.md")
        );

        let since = SystemTime::now() - Duration::from_secs(5);
        let mut line = serde_json::json!({ "paneId": "8", "reason": "finished", "seq": 2 });
        let event = finish_wait_line(&mut line, &results, since, &HashMap::new()).unwrap();
        assert!(line.get("result").is_none());
        assert_eq!(event["pane"], 8);
        assert_eq!(event["reason"], "finished");
        assert_eq!(event["seq"], 2);

        std::fs::write(&path, "report").unwrap();
        let mut line = serde_json::json!({ "paneId": "8", "reason": "finished", "seq": 2 });
        finish_wait_line(&mut line, &results, since, &HashMap::new()).unwrap();
        assert_eq!(line["result"]["bytes"], 6);
        // A timeout line is no outcome.
        let mut timeout = serde_json::json!({ "timeout": true, "pending": ["8"] });
        assert!(finish_wait_line(&mut timeout, &results, since, &HashMap::new()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Review finding #4: a `wait P --after SEQ` resuming a killed `send
    // --wait` left out "result" for a report written before it began.
    #[test]
    fn a_resumed_wait_counts_a_result_written_since_the_send_it_resumes() {
        let dir = temp_dir("wait-resumed");
        let path = result::prepare(&dir, Some("fix")).unwrap();
        std::fs::write(&path, "report").unwrap();
        let results = HashMap::from([(8, path)]);
        let sent = journal::now_secs() - 30;
        let events = journal::parse_events(&format!(
            "{{\"ev\":\"send\",\"ts\":{},\"pane\":8,\"seqBefore\":1,\"text\":\"a\"}}\n\
             {{\"ev\":\"send\",\"ts\":{sent},\"pane\":8,\"seqBefore\":2,\"text\":\"b\"}}\n\
             {{\"ev\":\"send\",\"ts\":{sent},\"pane\":9,\"seqBefore\":3,\"text\":\"c\"}}\n",
            sent - 100
        ));
        let resumed = resumed_since(&events, &HashMap::from([(8, 2), (7, 2)]));
        assert_eq!(
            resumed,
            HashMap::from([(8, SystemTime::UNIX_EPOCH + Duration::from_secs(sent))])
        );
        // The wait began after the report was written.
        let started = SystemTime::now() + Duration::from_secs(5);
        let line = || serde_json::json!({ "paneId": "8", "reason": "finished", "seq": 3 });
        let mut plain = line();
        finish_wait_line(&mut plain, &results, started, &HashMap::new()).unwrap();
        assert!(plain.get("result").is_none());
        let mut resumed_line = line();
        finish_wait_line(&mut resumed_line, &results, started, &resumed).unwrap();
        assert_eq!(resumed_line["result"]["bytes"], 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn result_parses_path_and_refuses_it_with_max_bytes() {
        let plain = Command::Result {
            pane: 4,
            max_bytes: None,
            path: false,
        };
        assert_eq!(parse_args(&args(&["result", "4"])).unwrap(), plain);
        let only = Command::Result {
            pane: 4,
            max_bytes: None,
            path: true,
        };
        assert_eq!(parse_args(&args(&["result", "4", "--path"])).unwrap(), only);
        assert_eq!(parse_args(&args(&["result", "--path", "4"])).unwrap(), only);
        for both in [
            &["result", "4", "--path", "--max-bytes", "10"][..],
            &["result", "4", "--max-bytes", "10", "--path"],
            &["result", "4", "--path", "--max-bytes", "bad"],
            &["result", "4", "--max-bytes=bad", "--path"],
        ] {
            let err = parse_args(&args(both)).unwrap_err();
            assert!(err.contains("--path and --max-bytes conflict"), "{err}");
        }
        assert!(parse_args(&args(&["result", "--path"])).is_err());
    }

    // Audit finding (AU-04): after its context was cut, an orchestrator
    // could not tell which task, tier or answer went to which worker.
    #[test]
    fn status_joins_each_worker_with_its_journal_and_lists_gone_ones_last() {
        let events = journal::parse_events(concat!(
            r#"{"ev":"spawn","ts":1,"pane":4,"label":"fix","argv":["claude"],"tier":"light","worktree":"/r-wt/fix","branch":"orch/fix","result":"/r-wt/fix/.noa/result-fix.md"}"#,
            "\n",
            r#"{"ev":"send","ts":2,"pane":4,"seqBefore":0,"text":"do it","answer":false}"#,
            "\n",
            r#"{"ev":"spawn","ts":3,"pane":6,"label":"old","argv":["codex"],"cwd":"/r"}"#,
            "\n",
            r#"{"ev":"close","ts":4,"pane":6}"#,
            "\n",
        ));
        let workers = journal::workers(&events);
        let mut fix = worker(4, 3);
        fix.label = Some("fix".to_string());
        fix.agent.as_mut().unwrap().blocked = Some("question".to_string());
        let mut plain = worker(5, 3);
        plain.agent = None;
        plain.spawn = Some(noa_ipc::PanelSpawn {
            argv: vec!["make".to_string()],
            started_at: 1,
        });
        let mut asked = Vec::new();
        let lines = status_lines(&[fix, plain], &workers, |branch, dir| {
            asked.push(format!("{branch} {dir}"));
            (Some(2), Some(false))
        });
        assert_eq!(asked, vec!["orch/fix /r-wt/fix".to_string()]);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["paneId"], "4");
        assert_eq!(lines[0]["blocked"], "question");
        assert_eq!(lines[0]["tier"], "light");
        assert_eq!(lines[0]["lastTask"], "do it");
        assert_eq!(lines[0]["ahead"], 2);
        assert_eq!(lines[0]["dirty"], false);
        assert_eq!(lines[0]["result"], "/r-wt/fix/.noa/result-fix.md");
        // Not in the journal: the server's argv, no journal fields.
        assert_eq!(lines[1]["argv"], serde_json::json!(["make"]));
        assert!(lines[1].get("answers").is_none());
        assert!(lines[1].get("ahead").is_none());
        assert_eq!(lines[2]["paneId"], "6");
        assert_eq!(lines[2]["gone"], true);
        assert_eq!(lines[2]["closed"], true);
        assert_eq!(lines[2]["label"], "old");
        assert_eq!(lines[2]["cwd"], "/r");

        assert_eq!(
            parse_args(&args(&["status", "--answers"])).unwrap(),
            Command::Status { answers: true }
        );
        assert_eq!(
            parse_args(&args(&["result", "4", "--max-bytes", "10"])).unwrap(),
            Command::Result {
                pane: 4,
                max_bytes: Some(10),
                path: false
            }
        );
        assert!(parse_args(&args(&["result"])).is_err());
        assert_eq!(last_lines("a\n\nb\n  \nc\n\n", 2), "b\nc");
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

    fn real_dirs(name: &str) -> std::path::PathBuf {
        let base = temp_dir(name).canonicalize().unwrap();
        for dir in ["sub", "other", "a/b", "real/sub"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        base
    }

    fn has_dots(path: &str) -> bool {
        path.split('/')
            .any(|segment| segment == "." || segment == "..")
    }

    #[test]
    fn dots_in_a_cwd_are_removed_and_name_the_same_directory() {
        let base = real_dirs("dots");
        for (dir, expect) in [
            ("sub/../other", base.join("other")),
            ("./sub", base.join("sub")),
            (".", base.clone()),
            ("a/./b", base.join("a/b")),
            ("a/b/../..", base.clone()),
        ] {
            let got = spawn_cwd(Some(dir.to_string()), Some(base.clone())).unwrap();
            assert!(!has_dots(&got), "{dir}: {got}");
            assert_eq!(std::path::Path::new(&got), expect, "{dir}");
            assert_eq!(
                std::path::Path::new(&got).canonicalize().unwrap(),
                base.join(dir).canonicalize().unwrap()
            );
        }
        let parent = spawn_cwd(Some("..".to_string()), Some(base.join("sub"))).unwrap();
        assert_eq!(std::path::Path::new(&parent), base);
        let absolute = format!("{}/a/../other", base.display());
        let got = spawn_cwd(Some(absolute), None).unwrap();
        assert_eq!(std::path::Path::new(&got), base.join("other"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_cwd_without_dots_is_unchanged() {
        let base = real_dirs("nodots");
        let trailing = format!("{}/sub/", base.display());
        assert_eq!(
            spawn_cwd(Some(trailing.clone()), Some(base.clone())),
            Some(trailing)
        );
        assert_eq!(
            spawn_cwd(Some("/repo".to_string()), Some(base.clone())).as_deref(),
            Some("/repo")
        );
        assert_eq!(
            spawn_cwd(Some("sub".to_string()), None).as_deref(),
            Some("sub")
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_dotdot_after_a_symlink_names_the_kernels_directory() {
        let base = real_dirs("symlink");
        std::os::unix::fs::symlink(base.join("real/sub"), base.join("link")).unwrap();
        let got = spawn_cwd(Some("link/..".to_string()), Some(base.clone())).unwrap();
        assert_eq!(std::path::Path::new(&got), base.join("real"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_missing_dotdot_cwd_is_returned_unchanged_and_refused() {
        let base = real_dirs("missing");
        let got = spawn_cwd(Some("nope/../sub".to_string()), Some(base.clone())).unwrap();
        assert_eq!(got, format!("{}/nope/../sub", base.display()));
        let err = check_spawn(
            &spawn_args(Some("nope/../sub"), &[]),
            Some(base.clone()),
            None,
        )
        .unwrap_err();
        assert!(err.ends_with("is not a directory"), "{err}");
        let _ = std::fs::remove_dir_all(base);
    }

    // Review finding: `check_spawn` judged the normalized path, so
    // `file/../dir` (refused by the kernel with ENOTDIR) passed as `dir`.
    #[test]
    fn a_dotdot_through_a_file_is_refused_though_it_normalizes_to_a_directory() {
        let base = real_dirs("throughfile");
        std::fs::write(base.join("file"), "").unwrap();

        let given = "file/../other";
        // macOS's realpath resolves `file/..`, so it normalizes to the
        // directory `other`; glibc's refuses it with ENOTDIR, so it comes
        // back unchanged. Either way `check_spawn` must refuse it.
        let got = spawn_cwd(Some(given.to_string()), Some(base.clone())).unwrap();
        assert!(
            std::path::Path::new(&got) == base.join("other")
                || std::path::Path::new(&got) == base.join(given),
            "{got}"
        );
        let err = check_spawn(&spawn_args(Some(given), &[]), Some(base.clone()), None).unwrap_err();
        assert!(
            err.starts_with(r#"--cwd "file/../other" (resolved to "#),
            "{err}"
        );
        assert!(err.ends_with("is not a directory"), "{err}");
        // The same path through a directory is still accepted.
        assert_eq!(
            check_spawn(
                &spawn_args(Some("sub/../other"), &[]),
                Some(base.clone()),
                None
            ),
            Ok(())
        );
        let _ = std::fs::remove_dir_all(base);
    }

    // As above for `locked/.`: no search permission on `locked`, so the
    // kernel refuses the path though `locked` itself is a directory.
    #[cfg(unix)]
    #[test]
    fn a_dot_inside_a_directory_without_search_permission_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let base = real_dirs("locked");
        let locked = base.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
        let args = spawn_args(Some("locked/."), &[]);
        let refused = check_spawn(&args, Some(base.clone()), None);
        let reachable = std::fs::metadata(locked.join(".")).is_ok();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&base);
        if reachable {
            // Running as root (or a filesystem ignoring modes): the
            // permission cannot be taken away here.
            return;
        }
        let err = refused.unwrap_err();
        assert!(err.ends_with("is not a directory"), "{err}");
    }

    // Acceptance criterion 1: the spawn line carries the resolved cwd.
    #[test]
    fn the_spawn_line_carries_the_cwd() {
        let line = spawn_line(7, false, None, None, None, Some("/repo/sub"));
        assert_eq!(line["cwd"], "/repo/sub");
        assert_eq!(line["paneId"], "7");

        let bare = spawn_line(7, false, None, None, None, None);
        assert!(bare.get("cwd").is_none(), "{bare}");

        let full = spawn_line(
            8,
            true,
            Some((std::path::Path::new("/wt"), "b")),
            Some("light"),
            Some("/wt/.noa/result-x.md"),
            Some("/wt"),
        );
        assert_eq!(full["cwd"], "/wt");
        assert_eq!(full["worktree"], "/wt");
        assert_eq!(full["branch"], "b");
        assert_eq!(full["tier"], "light");
        assert_eq!(full["result"], "/wt/.noa/result-x.md");
        assert_eq!(full["recovered"], true);
    }

    #[test]
    fn the_result_file_of_a_normalized_cwd_has_no_dots() {
        let base = real_dirs("result");
        let cwd = spawn_cwd(Some("sub/../other".to_string()), Some(base.clone())).unwrap();
        let path = result::prepare(std::path::Path::new(&cwd), Some("x")).unwrap();
        assert!(!has_dots(&path.to_string_lossy()), "{path:?}");
        assert!(path.starts_with(base.join("other")), "{path:?}");

        let mut p = panel(1, None, false);
        p.cwd = cwd.clone();
        p.label = Some("x".to_string());
        assert_eq!(
            result_path(&[], &p),
            Some(base.join("other/.noa/result-x.md"))
        );
        let _ = std::fs::remove_dir_all(base);
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
            ready: None,
            task: None,
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

    // Review finding: a program given by its absolute path
    // (`/opt/bin/claude`) was not checked, so `retry` closed the old worker
    // for a spawn that failed.
    #[cfg(unix)]
    #[test]
    fn a_program_given_by_path_must_be_an_executable_file() {
        use std::os::unix::fs::PermissionsExt;
        let bin = temp_dir("bypath");
        let tool = bin.join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(bin.join("data"), "").unwrap();
        let check = |command: &str| {
            check_spawn(
                &spawn_args(None, &[command]),
                Some(bin.clone()),
                Some("/usr/bin"),
            )
        };
        assert_eq!(check(&tool.to_string_lossy()), Ok(()));
        let gone = bin.join("gone").to_string_lossy().into_owned();
        assert_eq!(
            check(&gone),
            Err(format!(
                "command not found: {gone:?} is not an executable file"
            ))
        );
        assert!(check(&bin.join("data").to_string_lossy()).is_err());
        let _ = std::fs::remove_dir_all(bin);
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

    // Audit finding (REL-02): spawns run in parallel each counted the
    // workers before the others started, and together went past the limit.
    // The server now refuses at the limit (-32011), said as the client
    // says it, and a `--worktree` spawn counts that as refused.
    #[test]
    fn a_spawn_the_server_refused_at_the_limit_says_so_like_the_client() {
        let mut exited = worker(5, 3);
        exited.exit = Some(PanelExit { code: Some(0) });
        let panels = vec![worker(4, 3), exited, worker(6, 9)];
        assert_eq!(children_counts(&panels, 3), (2, 1));

        let refused = || ClientError::Rpc {
            code: RPC_LIMIT_REACHED,
            message: "pane 3 already has 2 children (1 live, 1 exited); maxChildren is 2"
                .to_string(),
        };
        let said = limit_reached_said(refused(), 3, 2, || Some(panels.clone()));
        let failure = Failure::from(said);
        assert_eq!(failure.code, EXIT_ERROR);
        assert_eq!(failure.message, max_children_message(2, 1, 2));
        // Without a listing, the server's own words.
        let failure = Failure::from(limit_reached_said(refused(), 3, 2, || None));
        assert!(failure.message.starts_with("pane 3 already has 2 children"));
        // Other errors pass through untouched.
        assert!(matches!(
            limit_reached_said(outcome_unknown(), 3, 2, || panic!("not listed")),
            ClientError::Rpc {
                code: RPC_OUTCOME_UNKNOWN,
                ..
            }
        ));

        let (result, log) = run_spawn_steps(false, Some(refused()));
        assert!(result.is_err());
        assert!(log.contains(&"undo wt refused=true".to_string()), "{log:?}");
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

    /// A [`FakeSource`] whose every pane shows `screen`.
    struct WithScreen(FakeSource, String);

    impl WaitSource for WithScreen {
        fn list(&mut self) -> Result<Vec<Panel>, Failure> {
            self.0.list()
        }

        fn next(&mut self, timeout: Duration) -> Result<Option<Notification>, Failure> {
            self.0.next(timeout)
        }

        fn scrollback(&mut self, pane: u64) -> Result<Option<String>, Failure> {
            self.0.scrollback(pane)
        }

        fn screen(&mut self, _pane: u64) -> Result<Option<String>, Failure> {
            Ok(Some(self.1.clone()))
        }
    }

    const BOX_RULE: &str = "────────────────────────────────────────";

    // Audit finding (AU-11): a `permission` line did not say what was
    // asked, and an agent's `--tail` ended in its input box and status line.
    #[test]
    fn a_question_line_shows_the_prompt_and_an_agents_tail_skips_its_input_box() {
        let screen = format!(
            "● Reading a.rs\n{BOX_RULE}\n Allow edit of a.rs?\n ❯ 1. Yes\n   2. No\n{BOX_RULE}\n\n"
        );
        let run = |state: &str, tail: Option<usize>, agent: bool| {
            let mut asked = with_pane(panel(2, Some(state), false), 8);
            if !agent {
                asked.agent.as_mut().unwrap().kind = None;
            }
            let mut source = WithScreen(
                FakeSource {
                    lists: vec![vec![asked]],
                    notifications: Default::default(),
                    list_calls: 0,
                    texts: HashMap::from([(8, format!("one\ntwo\n{screen}"))]),
                },
                screen.clone(),
            );
            let mut lines = Vec::new();
            let options = WaitOptions {
                tail,
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
            lines.remove(0)
        };
        let line = run("permission", None, true);
        assert_eq!(line["prompt"], " Allow edit of a.rs?\n ❯ 1. Yes\n   2. No");
        let line = run("input", Some(2), true);
        assert!(line.get("prompt").is_some());
        assert!(line["tail"].as_str().unwrap().contains("2. No"));

        let line = run("finished", Some(2), true);
        assert!(line.get("prompt").is_none());
        assert_eq!(line["tail"], "two\n● Reading a.rs");
        // A plain command keeps the raw end.
        let line = run("finished", Some(1), false);
        assert_eq!(line["tail"], BOX_RULE);
    }

    /// A [`FakeSource`] whose scrollback reads fail with `error`.
    struct TailFails(FakeSource, fn() -> ClientError);

    impl WaitSource for TailFails {
        fn list(&mut self) -> Result<Vec<Panel>, Failure> {
            self.0.list()
        }

        fn next(&mut self, timeout: Duration) -> Result<Option<Notification>, Failure> {
            self.0.next(timeout)
        }

        fn scrollback(&mut self, _pane: u64) -> Result<Option<String>, Failure> {
            Err(Failure::from((self.1)()))
        }

        fn screen(&mut self, pane: u64) -> Result<Option<String>, Failure> {
            self.0.screen(pane)
        }
    }

    /// A [`FakeSource`] that reports a caught signal once `after` calls to
    /// `next` were made.
    struct Interrupted {
        inner: FakeSource,
        nexts: usize,
        after: usize,
    }

    impl WaitSource for Interrupted {
        fn list(&mut self) -> Result<Vec<Panel>, Failure> {
            self.inner.list()
        }

        fn next(&mut self, timeout: Duration) -> Result<Option<Notification>, Failure> {
            self.nexts += 1;
            self.inner.next(timeout)
        }

        fn scrollback(&mut self, pane: u64) -> Result<Option<String>, Failure> {
            self.inner.scrollback(pane)
        }

        fn screen(&mut self, pane: u64) -> Result<Option<String>, Failure> {
            self.inner.screen(pane)
        }

        fn interrupted(&mut self) -> bool {
            self.nexts >= self.after
        }
    }

    // Audit finding (AU-08, REL-11): a wait killed by its shell tool printed
    // nothing, so the orchestrator had no baseline to carry on from.
    #[test]
    fn a_caught_signal_ends_the_wait_with_its_baselines() {
        let mut running = with_pane(panel(5, Some("running"), false), 8);
        running.label = Some("api".to_string());
        let mut source = Interrupted {
            inner: FakeSource {
                lists: vec![vec![running.clone()]],
                notifications: [Notification::StateChanged {
                    panels: vec![running],
                    removed_pane_ids: Vec::new(),
                    dropped: false,
                }]
                .into(),
                list_calls: 0,
                texts: HashMap::new(),
            },
            nexts: 0,
            after: 1,
        };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Listed {
                after: HashMap::from([(8, 4)]),
            },
            &WaitOptions::default(),
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_TIMEOUT));
        assert_eq!(
            lines,
            vec![serde_json::json!({
                "timeout": true,
                "interrupted": true,
                "pending": ["8"],
                "baseline": [{"paneId": "8", "seq": 4, "label": "api", "state": "running"}],
            })]
        );
    }

    /// A source whose every listing times out, with a signal caught once
    /// `after` listings were asked for.
    struct ListingTimesOut {
        lists: usize,
        after: usize,
    }

    impl WaitSource for ListingTimesOut {
        fn list(&mut self) -> Result<Vec<Panel>, Failure> {
            self.lists += 1;
            assert!(self.lists < 100, "the wait kept listing after the signal");
            Err(Failure::from(ClientError::Timeout(
                "noa.listPanels".to_string(),
            )))
        }

        fn next(&mut self, _timeout: Duration) -> Result<Option<Notification>, Failure> {
            unreachable!("no notification before the first listing")
        }

        fn scrollback(&mut self, _pane: u64) -> Result<Option<String>, Failure> {
            unreachable!()
        }

        fn screen(&mut self, _pane: u64) -> Result<Option<String>, Failure> {
            unreachable!()
        }

        fn interrupted(&mut self) -> bool {
            self.lists >= self.after
        }
    }

    // Review finding #8: with every listing timing out (a wedged Noa), the
    // first-listing retry loop never looked at a caught signal, so Ctrl-C
    // and SIGTERM could not end the wait.
    #[test]
    fn a_caught_signal_ends_a_wait_still_without_its_first_listing() {
        let mut source = ListingTimesOut { lists: 0, after: 3 };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &[8, 9],
            WaitStart::Listed {
                after: HashMap::from([(8, 4)]),
            },
            &WaitOptions::default(),
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_TIMEOUT));
        assert_eq!(source.lists, 3);
        assert_eq!(
            lines,
            vec![serde_json::json!({
                "timeout": true,
                "interrupted": true,
                "pending": ["8", "9"],
                "baseline": [{"paneId": "8", "seq": 4}],
            })]
        );
    }

    // Review finding: a wait's listings before its loop (the named wait's
    // guard, the result paths) neither retried a timed-out listing nor
    // looked at a caught signal: a timeout exited 3 and a signal ended the
    // process without the interrupted line.
    #[test]
    fn a_listing_before_the_wait_loop_retries_and_honours_a_signal() {
        let start = WaitStart::Named {
            after: HashMap::from([(8, 4)]),
        };
        let mut source = ListingTimesOut { lists: 0, after: 3 };
        let mut lines = Vec::new();
        let listed = list_before_wait(&mut source, &start, &[8, 9], None, &mut |line| {
            lines.push(line.clone())
        });
        assert!(
            matches!(listed, Ok(None)),
            "{:?}",
            listed.map_err(|f| f.message)
        );
        assert_eq!(source.lists, 3);
        assert_eq!(
            lines,
            vec![serde_json::json!({
                "timeout": true,
                "interrupted": true,
                "pending": ["8", "9"],
                "baseline": [{"paneId": "8", "seq": 4}],
            })]
        );

        // A timeout is retried until a listing comes back.
        let mut source = FakeSource {
            lists: vec![vec![panel(1, Some("running"), false)]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut flaky = Flaky {
            inner: &mut source,
            fail: 2,
        };
        let listed = list_before_wait(&mut flaky, &start, &[8], None, &mut |_| {}).unwrap();
        assert_eq!(listed.map(|panels| panels.len()), Some(1));

        // Past the wait's deadline it ends as the first listing does.
        let mut flaky = Flaky {
            inner: &mut source,
            fail: usize::MAX,
        };
        let failure = list_before_wait(&mut flaky, &start, &[8], Some(Instant::now()), &mut |_| {})
            .unwrap_err();
        assert_eq!(failure.code, EXIT_UNAVAILABLE);
    }

    /// A source whose first `fail` listings time out.
    struct Flaky<'a> {
        inner: &'a mut FakeSource,
        fail: usize,
    }

    impl WaitSource for Flaky<'_> {
        fn list(&mut self) -> Result<Vec<Panel>, Failure> {
            if self.fail > 0 {
                self.fail -= 1;
                return Err(Failure::from(ClientError::Timeout(
                    "noa.listPanels".to_string(),
                )));
            }
            self.inner.list()
        }

        fn next(&mut self, timeout: Duration) -> Result<Option<Notification>, Failure> {
            self.inner.next(timeout)
        }

        fn scrollback(&mut self, pane: u64) -> Result<Option<String>, Failure> {
            self.inner.scrollback(pane)
        }

        fn screen(&mut self, pane: u64) -> Result<Option<String>, Failure> {
            self.inner.screen(pane)
        }
    }

    // Audit finding (REL-09): `wait PANE` checked no parent, so after a
    // restart gave pane ids out again, a remembered worker id waited on an
    // unrelated pane.
    #[test]
    fn a_named_wait_refuses_a_listed_pane_that_is_not_a_worker() {
        let panels = vec![worker(8, 1), worker(5, 2), worker(1, 0)];
        assert_eq!(wait_guard(&panels, &[8], 1), Ok(()));
        assert_eq!(wait_guard(&panels, &[8, 5], 1), Err(not_worker_message(5)));
        // This pane itself, and a pane that is not listed (the wait says so).
        assert_eq!(wait_guard(&panels, &[1, 9999], 1), Ok(()));
        let Command::Wait(wait) = parse_args(&args(&["wait", "5", "--any-pane"])).unwrap() else {
            panic!("wait expected");
        };
        assert!(wait.any_pane);
    }

    // Audit finding (REL-05): a `--tail` read past the client's deadline
    // ended the wait with exit 3 and lost the outcome it had reached.
    #[test]
    fn a_tail_that_cannot_be_read_keeps_the_outcome() {
        for error in [
            (|| ClientError::Timeout("noa.getText".to_string())) as fn() -> ClientError,
            || ClientError::Transport("connection reset".to_string()),
        ] {
            let mut source = TailFails(
                FakeSource {
                    lists: vec![vec![with_pane(panel(2, Some("finished"), false), 8)]],
                    notifications: Default::default(),
                    list_calls: 0,
                    texts: HashMap::new(),
                },
                error,
            );
            let mut lines = Vec::new();
            let code = wait_loop(
                &mut source,
                &[8],
                WaitStart::Listed {
                    after: HashMap::from([(8, 1)]),
                },
                &WaitOptions {
                    tail: Some(5),
                    ..WaitOptions::default()
                },
                WAIT_RELIST_INTERVAL,
                &mut |line| lines.push(line.clone()),
            );
            assert_eq!(code.ok(), Some(EXIT_OK));
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0]["reason"], "finished");
            assert!(lines[0].get("tail").is_none());
            assert!(
                lines[0]["tailError"]
                    .as_str()
                    .is_some_and(|error| !error.is_empty()),
                "{}",
                lines[0]
            );
        }
    }

    // Audit finding (REL-12): an agent that quit back to its shell cleared
    // its state without a new report, so the wait ran to its timeout.
    #[test]
    fn an_agent_that_quit_to_its_shell_ends_the_wait() {
        let running = panel(4, Some("running"), false);
        let mut watch = Watch::new(8, Some(&running), None);
        assert!(watch.had_agent);
        assert_eq!(evaluate(&mut watch, Some(&running), &Reason::ALL), None);

        let mut shell = panel(4, None, false);
        shell.agent.as_mut().unwrap().kind = None;
        let outcome = evaluate(&mut watch, Some(&shell), &[Reason::Finished]).unwrap();
        assert_eq!(
            outcome,
            Outcome {
                reason: Reason::Exit,
                closed: false,
                agent_left: true,
            }
        );
        let line = outcome_json(&watch, &outcome, Some(&shell), None);
        assert_eq!(line["reason"], "exit");
        assert_eq!(line["agentLeft"], true);
        assert!(line.get("closed").is_none());
        shell.agent = None;
        assert!(evaluate(&mut watch, Some(&shell), &Reason::ALL).is_some_and(|o| o.agent_left));

        // A pane that had no agent when the wait began is a shell's wait.
        let mut watch = Watch::new(8, Some(&shell), None);
        assert_eq!(evaluate(&mut watch, Some(&shell), &Reason::ALL), None);
        // A report left after the agent's process went is still waited on.
        let mut reported = panel(4, Some("running"), false);
        reported.agent.as_mut().unwrap().kind = None;
        let mut watch = Watch::new(8, Some(&running), None);
        assert_eq!(evaluate(&mut watch, Some(&reported), &Reason::ALL), None);
    }

    // Audit finding (AU-04): a wait's lines named panes by id only, so an
    // orchestrator that lost track of its ids could not tell its workers
    // apart.
    #[test]
    fn wait_lines_carry_the_panes_label() {
        let mut labelled = with_pane(panel(3, Some("running"), false), 8);
        labelled.label = Some("api".to_string());
        let unlabelled = with_pane(panel(1, Some("running"), false), 9);
        let mut source = FakeSource {
            lists: vec![vec![labelled.clone(), unlabelled.clone()]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &[8, 9],
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
            lines[0]["baseline"],
            serde_json::json!([
                {"paneId": "8", "seq": 3, "label": "api", "state": "running"},
                {"paneId": "9", "seq": 1, "state": "running"},
            ])
        );

        // On an outcome, and still once the pane is gone.
        let mut finished = with_pane(panel(4, Some("finished"), false), 8);
        finished.label = Some("api".to_string());
        let mut source = FakeSource {
            lists: vec![vec![labelled.clone()], Vec::new()],
            notifications: [Notification::StateChanged {
                panels: vec![labelled],
                removed_pane_ids: Vec::new(),
                dropped: false,
            }]
            .into(),
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
        assert_eq!(lines[0]["closed"], true);
        assert_eq!(lines[0]["label"], "api");
        let watch = Watch::new(8, Some(&finished), Some(3));
        let outcome = Outcome {
            reason: Reason::Finished,
            closed: false,
            agent_left: false,
        };
        assert_eq!(
            outcome_json(&watch, &outcome, Some(&finished), None)["label"],
            "api"
        );
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

    /// A `--settle` wait on pane 8 from report count 1: `lists[N]` and
    /// `screens[N]` are what second N shows. Returns each line with the
    /// second it was printed at.
    fn run_settle_wait(
        lists: Vec<Vec<Panel>>,
        screens: Vec<&'static str>,
        settle_secs: u64,
    ) -> (Result<i32, Failure>, Vec<(u64, serde_json::Value)>) {
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
            during_screen: None,
        };
        let base = Instant::now();
        let mut lines = Vec::new();
        // Bounded, so a wait that would never end shows as a timeout.
        let options = WaitOptions {
            settle: Some(Duration::from_secs(settle_secs)),
            timeout: Some(Duration::from_secs(30)),
            ..WaitOptions::default()
        };
        let clock_now = now.clone();
        let code = wait_loop_with_clock(
            &mut source,
            &[8],
            WaitStart::Listed {
                after: HashMap::from([(8, 1)]),
            },
            &options,
            Duration::ZERO,
            &mut |line| lines.push((now.get(), line.clone())),
            &mut || base + Duration::from_secs(clock_now.get()),
        );
        (code, lines)
    }

    // Audit finding (AU-12): an agent that reported `finished` went on
    // working, and the orchestrator took the early report for the end.
    #[test]
    fn a_settled_finish_is_printed_only_after_a_quiet_hold() {
        let finished = |seq| panel(seq, Some("finished"), false);
        let (code, lines) = run_settle_wait(vec![vec![finished(2)]], vec!["work"], 3);
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines.len(), 1);
        let (at, line) = &lines[0];
        assert_eq!(*at, 3);
        assert_eq!(line["reason"], "finished");
        assert_eq!(line["settledSecs"], 3);

        // A newer report during the hold drops it; the next finish holds.
        let (_, lines) = run_settle_wait(
            vec![
                vec![finished(2)],
                vec![panel(3, Some("running"), false)],
                vec![finished(4)],
            ],
            vec!["work"],
            3,
        );
        let (at, line) = &lines[0];
        assert_eq!(line["seq"], 4);
        assert_eq!(*at, 5);

        // Review finding #3: the work on screen changing starts the hold
        // again and keeps the report, so a one-off repaint only delays the
        // line (dropping it hung a wait that got no newer report).
        let (code, lines) = run_settle_wait(vec![vec![finished(2)]], vec!["a", "a", "b"], 3);
        assert_eq!(code.ok(), Some(EXIT_OK));
        let (at, line) = &lines[0];
        assert_eq!(line["reason"], "finished");
        assert_eq!(line["seq"], 2);
        assert_eq!(line["settledSecs"], 3);
        assert_eq!(*at, 5);
        // A newer report during the restarted hold still replaces it.
        let (_, lines) = run_settle_wait(
            vec![
                vec![finished(2)],
                vec![finished(2)],
                vec![finished(2)],
                vec![finished(2)],
                vec![finished(3)],
            ],
            vec!["a", "a", "b"],
            3,
        );
        let (at, line) = &lines[0];
        assert_eq!(line["seq"], 3);
        assert_eq!(*at, 7);

        // Any other reason ends the wait at once.
        let (_, lines) = run_settle_wait(
            vec![vec![finished(2)], vec![panel(3, Some("permission"), false)]],
            vec!["work"],
            3,
        );
        let (at, line) = &lines[0];
        assert_eq!(line["reason"], "permission");
        assert!(line.get("settledSecs").is_none());
        assert_eq!(*at, 1);

        let Command::Wait(wait) = parse_args(&args(&["wait", "8", "--settle", "1.5"])).unwrap()
        else {
            panic!("wait expected");
        };
        assert_eq!(wait.options.settle, Some(Duration::from_millis(1500)));
        assert_eq!(secs_json(Duration::from_millis(1500)), 1.5);
        assert!(parse_args(&args(&["wait", "8", "--settle", "0"])).is_err());
        assert!(
            send_args(&["--wait", "--settle", "10", "x"])
                .wait
                .unwrap()
                .settle
                .is_some()
        );
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
                    removed_pane_ids: Vec::new(),
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

    // Review finding: `wait --children --after 3=1,4=2` failed ("not waited
    // on") when worker 4 closed since the timeout line that gave its
    // baseline, breaking the resume loop. A closed one is reported closed;
    // a listed pane that is not a worker is still refused.
    #[test]
    fn a_children_wait_reports_an_after_pane_that_closed_since() {
        assert_eq!(closed_after_panes(&[3], &[(3, 1), (4, 2)], &[3]), [4]);
        assert!(closed_after_panes(&[3], &[(3, 1), (4, 2)], &[3, 4]).is_empty());
        assert!(wait_baselines(&[3], &[(3, 1), (4, 2)]).is_err());

        let panes = [3, 4];
        let after = wait_baselines(&panes, &[(3, 1), (4, 2)]).unwrap();
        let mut source = FakeSource {
            lists: vec![vec![with_pane(panel(1, Some("running"), false), 3)]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &panes,
            WaitStart::Listed { after },
            &WaitOptions::default(),
            WAIT_RELIST_INTERVAL,
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(
            lines,
            vec![serde_json::json!({"paneId": "4", "reason": "exit", "closed": true})]
        );
    }

    /// A [`FakeSource`] whose listings at the given call indexes fail with
    /// `error`, and whose screen reads time out when `screen_times_out`.
    struct FlakySource {
        inner: FakeSource,
        calls: usize,
        fail_at: Vec<usize>,
        error: fn() -> ClientError,
        screen_times_out: bool,
    }

    impl WaitSource for FlakySource {
        fn list(&mut self) -> Result<Vec<Panel>, Failure> {
            let call = self.calls;
            self.calls += 1;
            if self.fail_at.contains(&call) {
                return Err(Failure::from((self.error)()));
            }
            self.inner.list()
        }

        fn next(&mut self, timeout: Duration) -> Result<Option<Notification>, Failure> {
            self.inner.next(timeout)
        }

        fn scrollback(&mut self, pane: u64) -> Result<Option<String>, Failure> {
            self.inner.scrollback(pane)
        }

        fn screen(&mut self, pane: u64) -> Result<Option<String>, Failure> {
            if self.screen_times_out {
                return Err(Failure::from(ClientError::Timeout(
                    "noa.getText".to_string(),
                )));
            }
            self.inner.screen(pane)
        }
    }

    /// Pane 8 runs, then finishes; listings `fail_at` fail with `error`.
    fn run_flaky_wait(
        fail_at: Vec<usize>,
        error: fn() -> ClientError,
        screen_times_out: bool,
        options: WaitOptions,
    ) -> (Result<i32, Failure>, Vec<serde_json::Value>) {
        let running = with_pane(panel(1, Some("running"), false), 8);
        let mut source = FlakySource {
            inner: FakeSource {
                lists: vec![
                    vec![running.clone()],
                    vec![with_pane(panel(2, Some("finished"), false), 8)],
                ],
                notifications: (0..4)
                    .map(|_| Notification::StateChanged {
                        panels: vec![running.clone()],
                        removed_pane_ids: Vec::new(),
                        dropped: false,
                    })
                    .collect(),
                list_calls: 0,
                texts: HashMap::new(),
            },
            calls: 0,
            fail_at,
            error,
            screen_times_out,
        };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &[8],
            WaitStart::Listed {
                after: HashMap::new(),
            },
            &options,
            Duration::ZERO,
            &mut |line| lines.push(line.clone()),
        );
        (code, lines)
    }

    // Review finding: one request past the client's 5 s deadline ended a
    // long `wait` with exit 3 "lost the connection" and no timeout line,
    // though the connection was alive. It is retried on the next tick; a
    // lost connection still ends the wait.
    #[test]
    fn a_wait_retries_a_listing_that_timed_out() {
        let timeout = || ClientError::Timeout("noa.listPanels".to_string());
        // The first listing, and a re-list inside the wait.
        let (code, lines) = run_flaky_wait(vec![0, 2], timeout, false, WaitOptions::default());
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["reason"], "finished");

        let lost = || ClientError::Transport("connection reset".to_string());
        let (code, lines) = run_flaky_wait(vec![1], lost, false, WaitOptions::default());
        assert_eq!(
            code.err().map(|failure| failure.code),
            Some(EXIT_UNAVAILABLE)
        );
        assert!(lines.is_empty());

        // Outside the wait loop a timeout still ends a command with exit 3.
        let failure = Failure::from(timeout());
        assert_eq!(failure.code, EXIT_UNAVAILABLE);
        assert!(failure.transient);
        assert!(!Failure::from(lost()).transient);
    }

    #[test]
    fn a_stall_check_skips_a_screen_read_that_timed_out() {
        let options = WaitOptions {
            reasons: vec![Reason::Finished, Reason::Stalled],
            stall: Some(Duration::ZERO),
            ..WaitOptions::default()
        };
        let timeout = || ClientError::Timeout("noa.listPanels".to_string());
        let (code, lines) = run_flaky_wait(Vec::new(), timeout, true, options);
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(lines[0]["reason"], "finished");
    }

    // Review finding: a timed-out wait's `baseline` also listed panes it
    // reported under `exited`; it covers the pending panes only.
    #[test]
    fn a_timeout_baseline_leaves_out_exited_panes() {
        let mut exited = with_pane(panel(0, None, false), 6);
        exited.exit = Some(PanelExit { code: Some(0) });
        let mut source = FakeSource {
            lists: vec![vec![exited, with_pane(panel(4, Some("running"), false), 3)]],
            notifications: Default::default(),
            list_calls: 0,
            texts: HashMap::new(),
        };
        let mut lines = Vec::new();
        let code = wait_loop(
            &mut source,
            &[6, 3],
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
                "pending": ["3"],
                "exited": ["6"],
                "baseline": [{"paneId": "3", "seq": 4, "state": "running"}],
            })]
        );
    }

    /// Refuses every close as `PaneClosed`; listings after the first show
    /// none of the panes, as when they went away in between.
    struct VanishingCloser {
        panels: Vec<Panel>,
        lists: usize,
        code: i64,
    }

    impl PaneCloser for VanishingCloser {
        fn list(&mut self) -> Result<Vec<Panel>, ClientError> {
            self.lists += 1;
            Ok(if self.lists == 1 {
                self.panels.clone()
            } else {
                Vec::new()
            })
        }

        fn close(&mut self, _pane: u64) -> Result<(), ClientError> {
            Err(ClientError::Rpc {
                code: self.code,
                message: "pane closed".to_string(),
            })
        }
    }

    // Review finding: a worker that went away between the listing and the
    // close was reported gone but its worktree was not pruned, though its
    // label and cwd were already read.
    #[test]
    fn a_worker_gone_before_its_close_is_still_pruned() {
        let mut labelled = worker(8, 1);
        labelled.label = Some("job".to_string());
        let mut source = VanishingCloser {
            panels: vec![labelled],
            lists: 0,
            code: RPC_PANE_CLOSED,
        };
        let mut asked = Vec::new();
        let mut lines = Vec::new();
        let code = close_panes(
            &mut source,
            &[8],
            Some(1),
            &[],
            &mut |_pane, label, cwd| {
                asked.push((label.to_string(), cwd.to_string()));
                Some(serde_json::json!({ "path": "/r-wt/job", "removed": true }))
            },
            &mut |line| lines.push(line.clone()),
        );
        assert_eq!(code.ok(), Some(EXIT_OK));
        assert_eq!(asked, [("job".to_string(), "/repo".to_string())]);
        assert_eq!(lines[0]["gone"], true);
        assert_eq!(lines[0]["worktree"]["removed"], true);
    }

    /// A writer whose reader went away.
    struct ClosedPipe(usize);

    impl Write for ClosedPipe {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            self.0 += 1;
            Err(std::io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // Review finding: `println!` panicked on EPIPE, so `close --children |
    // head -1` stopped after the first line and left workers open.
    #[test]
    fn a_closed_stdout_does_not_stop_the_command() {
        let mut pipe = ClosedPipe(0);
        write_json_line(&mut pipe, &serde_json::json!({ "paneId": "8" }));
        write_json_line(&mut pipe, &serde_json::json!({ "paneId": "9" }));
        assert_eq!(pipe.0, 2);

        let mut out = Vec::new();
        write_json_line(&mut out, &serde_json::json!({ "paneId": "8" }));
        assert_eq!(out, b"{\"paneId\":\"8\"}\n");
    }

    // Review finding: a Noa that reports no surface ids failed a command
    // that targets workers (`guard_own`) with exit 1, while `require_own`
    // and the reference give exit 3 for it.
    #[test]
    fn a_noa_without_surface_ids_is_unavailable_for_every_own_pane_check() {
        assert_eq!(
            own_missing_failure("m".to_string(), true).code,
            EXIT_UNAVAILABLE
        );
        assert_eq!(own_missing_failure("m".to_string(), false).code, EXIT_ERROR);
    }

    // Review finding: a mistyped id was "not a worker of this pane (pass
    // --any-pane …)", inviting `--any-pane` for a pane that does not exist.
    #[test]
    fn input_to_an_unlisted_pane_says_it_is_not_listed() {
        let panels = vec![worker(8, 1), worker(5, 2)];
        assert_eq!(
            guard_targets(&panels, &[9999], 1),
            Err("pane 9999 is not listed".to_string())
        );
        assert_eq!(guard_targets(&panels, &[5], 1), Err(not_worker_message(5)));
        assert_eq!(guard_targets(&panels, &[8], 1), Ok(()));
        // `close` of an unlisted pane still counts it as gone.
        assert_eq!(
            close_targets(&panels, &[9999], Some(1)),
            Ok(vec![(9999, false)])
        );
    }

    // Review finding: the app answering a spawn late (-32009) was read as a
    // refusal, and the worktree of a worker that was starting was removed.
    #[test]
    fn a_spawn_whose_outcome_is_unknown_keeps_its_worktree() {
        let (result, log) = run_spawn_steps(
            false,
            Some(ClientError::Rpc {
                code: RPC_OUTCOME_UNKNOWN,
                message: "outcome unknown: request timed out".to_string(),
            }),
        );
        assert!(result.is_err());
        assert_eq!(
            log.last().map(String::as_str),
            Some("undo wt refused=false")
        );
    }
}
