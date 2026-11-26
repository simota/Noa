//! `Noa +ctl timeline`: read and write a board's timeline
//! (`docs/specs/board-timeline.md` AC-2, L2-8) — `context`, `post`, `list`,
//! `show`, `changes`, `export`, `boards`, each printing one JSON document.

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use noa_ipc::timeline::{
    Artifact, BOARDS_MAX, BoardCursor, BoardsParams, BoardsResult, CHANGES_MAX, ChangesParams,
    CheckResult, ContextParams, ContextResult, Filter, GetParams, GetResult, IdKind, LIST_MAX,
    ListParams, MAX_BODY_BYTES, MAX_POST_PARAMS_BYTES, MAX_QUERY_CHARS, MAX_TOKEN_BYTES, Outcome,
    PageResult, Post, PostInput, PostKind, PostParams, PostResult, ProducerKind, Provenance,
    PublishOwner, PublishPolicy, RunInfo, SCHEMA_VERSION, Seq, SetPublishPolicyParams, SourceRef,
    TimelineErrorKind, UNREGISTERED_NAMESPACES, canon_bytes, valid_id, valid_token,
};
use noa_ipc::timeline_outbox::{Envelope, Outbox, OutboxError, PostSink, PublishState, publish};
use noa_ipc::{CAPABILITY_BOARD_TIMELINE_V1, Client, ClientError, WireId};
use serde::Serialize;

use super::{Args, EXIT_OK, Failure, parse_u64};

/// Default `producerId` for manual CLI imports when `--producer` is omitted.
const DEFAULT_MANUAL_PRODUCER: &str = "cli:manual";

/// Top-level JSON keys forbidden in `post --file F` because they belong to
/// the CLI flags/context envelope rather than the post body (§9).
const FORBIDDEN_FILE_KEYS: [&str; 4] = ["boardId", "runId", "producerId", "requestId"];

/// One `timeline` subcommand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TimelineCommand {
    Context {
        pane: Option<u64>,
    },
    Boards {
        before: Option<BoardCursor>,
        limit: Option<usize>,
    },
    Post(PostArgs),
    List {
        board: Option<String>,
        before: Option<u64>,
        limit: Option<usize>,
        filter: Filter,
        query: Option<String>,
    },
    Show {
        board: Option<String>,
        post: String,
    },
    Changes {
        board: Option<String>,
        after: u64,
        limit: Option<usize>,
    },
    Export {
        board: Option<String>,
        format: ExportFormat,
        output: PathBuf,
    },
}

/// `timeline post`'s options (`--file F`, or `--from-result PATH` with
/// `--title`/`--summary`/`--outcome`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PostArgs {
    pub(crate) board: Option<String>,
    pub(crate) run: Option<String>,
    pub(crate) producer: Option<String>,
    pub(crate) request_id: Option<String>,
    pub(crate) source: PostSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PostSource {
    File(PathBuf),
    FromResult {
        path: PathBuf,
        title: String,
        summary: String,
        outcome: Outcome,
        kind: PostKind,
        take_over: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExportFormat {
    Markdown,
    Json,
}

/// The timeline calls `+ctl timeline` makes: a [`Client`], or a fake in tests.
pub(crate) trait TimelineApi {
    fn supports(&self) -> bool;
    fn context(&mut self, p: &ContextParams) -> Result<ContextResult, ClientError>;
    fn boards(&mut self, p: &BoardsParams) -> Result<BoardsResult, ClientError>;
    fn set_publish_policy(&mut self, p: &SetPublishPolicyParams) -> Result<RunInfo, ClientError>;
    fn post(&mut self, p: &PostParams) -> Result<PostResult, ClientError>;
    fn list(&mut self, p: &ListParams) -> Result<PageResult, ClientError>;
    fn get(&mut self, p: &GetParams) -> Result<GetResult, ClientError>;
    fn changes(&mut self, p: &ChangesParams) -> Result<PageResult, ClientError>;
}

impl TimelineApi for Client {
    fn supports(&self) -> bool {
        self.supports_board_timeline()
    }

    fn context(&mut self, p: &ContextParams) -> Result<ContextResult, ClientError> {
        self.timeline_context(p)
    }

    fn boards(&mut self, p: &BoardsParams) -> Result<BoardsResult, ClientError> {
        self.timeline_boards(p)
    }

    fn set_publish_policy(&mut self, p: &SetPublishPolicyParams) -> Result<RunInfo, ClientError> {
        self.timeline_set_publish_policy(p)
    }

    fn post(&mut self, p: &PostParams) -> Result<PostResult, ClientError> {
        self.timeline_post(p)
    }

    fn list(&mut self, p: &ListParams) -> Result<PageResult, ClientError> {
        self.timeline_list(p)
    }

    fn get(&mut self, p: &GetParams) -> Result<GetResult, ClientError> {
        self.timeline_get(p)
    }

    fn changes(&mut self, p: &ChangesParams) -> Result<PageResult, ClientError> {
        self.timeline_changes(p)
    }
}

// ---------------------------------------------------------------------------
// Argument parsing

/// The arguments after `timeline`.
pub(crate) fn parse(args: &[String]) -> Result<TimelineCommand, String> {
    let (sub, rest) = args.split_first().ok_or(
        "missing timeline subcommand (expected context, boards, post, list, show, changes, export)",
    )?;
    let mut rest = Args::new(rest);
    let command = match sub.as_str() {
        "context" => {
            let pane = rest
                .value("--pane")?
                .map(|v| parse_u64("--pane", &v))
                .transpose()?;
            TimelineCommand::Context { pane }
        }
        "boards" => {
            let before = rest
                .value("--before")?
                .map(|v| parse_board_cursor(&v))
                .transpose()?;
            let limit = parse_limit(&mut rest, BOARDS_MAX)?;
            TimelineCommand::Boards { before, limit }
        }
        "post" => TimelineCommand::Post(parse_post(&mut rest)?),
        "list" => parse_list(&mut rest)?,
        "show" => parse_show(&mut rest)?,
        "changes" => parse_changes(&mut rest)?,
        "export" => parse_export(&mut rest)?,
        other => return Err(format!("unknown timeline subcommand: {other:?}")),
    };
    rest.finish()?;
    Ok(command)
}

fn parse_post(rest: &mut Args) -> Result<PostArgs, String> {
    let board = parse_opt_id(rest, "--board", IdKind::Board)?;
    let run = parse_opt_id(rest, "--run", IdKind::Run)?;
    let producer = parse_opt_token(rest, "--producer")?;
    let request_id = parse_opt_token(rest, "--request-id")?;
    let file = rest.value("--file")?;
    let from_result = rest.value("--from-result")?;
    let title = rest.value("--title")?;
    let summary = rest.value("--summary")?;
    let outcome = rest
        .value("--outcome")?
        .map(|v| parse_outcome(&v))
        .transpose()?;
    let kind = rest.value("--kind")?.map(|v| parse_kind(&v)).transpose()?;
    let take_over = rest.flag("--take-over");

    let source = match (file, from_result) {
        (Some(_), Some(_)) => {
            return Err("--file and --from-result are exclusive".to_string());
        }
        (None, None) => {
            return Err("post needs either --file FILE or --from-result PATH".to_string());
        }
        (Some(path), None) => {
            if title.is_some()
                || summary.is_some()
                || outcome.is_some()
                || kind.is_some()
                || take_over
            {
                return Err(
                    "--title, --summary, --outcome, --kind and --take-over go with --from-result, \
                     not --file"
                        .to_string(),
                );
            }
            if path.is_empty() {
                return Err("--file path must not be empty".to_string());
            }
            PostSource::File(PathBuf::from(path))
        }
        (None, Some(path)) => {
            if path.is_empty() {
                return Err("--from-result path must not be empty".to_string());
            }
            let title = title.ok_or("--from-result needs --title")?;
            let summary = summary.ok_or("--from-result needs --summary")?;
            let outcome = outcome.ok_or("--from-result needs --outcome")?;
            if take_over && run.is_none() {
                return Err("--take-over needs --run".to_string());
            }
            PostSource::FromResult {
                path: PathBuf::from(path),
                title,
                summary,
                outcome,
                kind: kind.unwrap_or(PostKind::Summary),
                take_over,
            }
        }
    };

    Ok(PostArgs {
        board,
        run,
        producer,
        request_id,
        source,
    })
}

fn parse_list(rest: &mut Args) -> Result<TimelineCommand, String> {
    let board = parse_opt_id(rest, "--board", IdKind::Board)?;
    let before = match (rest.value("--before")?, rest.value("--before-seq")?) {
        (Some(_), Some(_)) => return Err("--before and --before-seq are exclusive".to_string()),
        (Some(v), None) => Some(parse_u64("--before", &v)?),
        (None, Some(v)) => Some(parse_u64("--before-seq", &v)?),
        (None, None) => None,
    };
    let limit = parse_limit(rest, LIST_MAX)?;
    let query = rest.value("--query")?;
    if let Some(q) = &query
        && q.chars().count() > MAX_QUERY_CHARS
    {
        return Err(format!("--query is over {MAX_QUERY_CHARS} characters"));
    }

    let mut filter = Filter::default();
    while let Some(raw) = rest.value("--worker")? {
        for part in split_csv("--worker", &raw)? {
            check_id("--worker", IdKind::Worker, part)?;
            filter.worker_ids.push(part.to_string());
        }
    }
    while let Some(raw) = rest.value("--agent")? {
        for part in split_csv("--agent", &raw)? {
            filter.agent_kinds.push(part.to_string());
        }
    }
    while let Some(raw) = rest.value("--kind")? {
        for part in split_csv("--kind", &raw)? {
            filter.kinds.push(parse_kind(part)?);
        }
    }
    while let Some(raw) = rest.value("--outcome")? {
        for part in split_csv("--outcome", &raw)? {
            filter.outcomes.push(parse_outcome(part)?);
        }
    }

    Ok(TimelineCommand::List {
        board,
        before,
        limit,
        filter,
        query,
    })
}

fn parse_show(rest: &mut Args) -> Result<TimelineCommand, String> {
    let board = parse_opt_id(rest, "--board", IdKind::Board)?;
    let post_flag = rest.value("--post")?;
    let positionals = rest.positionals()?;
    let post = match (post_flag, positionals.as_slice()) {
        (Some(_), [_, ..]) => {
            return Err("show takes POST either positionally or with --post, not both".to_string());
        }
        (Some(id), []) => id,
        (None, [id]) => id.clone(),
        (None, []) => return Err("show needs POST (or --post POST)".to_string()),
        (None, _) => return Err("show takes exactly one POST".to_string()),
    };
    check_id("POST", IdKind::Post, &post)?;
    Ok(TimelineCommand::Show { board, post })
}

fn parse_changes(rest: &mut Args) -> Result<TimelineCommand, String> {
    let board = parse_opt_id(rest, "--board", IdKind::Board)?;
    let after = match (rest.value("--after")?, rest.value("--after-seq")?) {
        (Some(_), Some(_)) => return Err("--after and --after-seq are exclusive".to_string()),
        (Some(v), None) => parse_u64("--after", &v)?,
        (None, Some(v)) => parse_u64("--after-seq", &v)?,
        (None, None) => 0,
    };
    let limit = parse_limit(rest, CHANGES_MAX)?;
    Ok(TimelineCommand::Changes {
        board,
        after,
        limit,
    })
}

fn parse_export(rest: &mut Args) -> Result<TimelineCommand, String> {
    let board = parse_opt_id(rest, "--board", IdKind::Board)?;
    let format = match rest
        .value("--format")?
        .ok_or("export needs --format markdown|json")?
        .as_str()
    {
        "markdown" => ExportFormat::Markdown,
        "json" => ExportFormat::Json,
        other => {
            return Err(format!(
                "unknown --format {other:?} (expected markdown|json)"
            ));
        }
    };
    let output = rest
        .value("--output")?
        .ok_or("export needs --output PATH")?;
    if output.is_empty() {
        return Err("--output path must not be empty".to_string());
    }
    Ok(TimelineCommand::Export {
        board,
        format,
        output: PathBuf::from(output),
    })
}

/// `boards --before ACTIVITY_AT,BOARD_ID`: a previous page's `nextBefore`
/// (`activityAt` and `boardId` joined by a comma; neither contains one).
/// The server checks `activityAt` is RFC 3339 UTC.
fn parse_board_cursor(raw: &str) -> Result<BoardCursor, String> {
    let (activity_at, board_id) = raw
        .split_once(',')
        .filter(|(at, _)| !at.is_empty())
        .ok_or_else(|| {
            format!("--before {raw:?} is not ACTIVITY_AT,BOARD_ID (a previous page's nextBefore)")
        })?;
    check_id("--before board", IdKind::Board, board_id)?;
    Ok(BoardCursor {
        activity_at: activity_at.to_string(),
        board_id: board_id.to_string(),
    })
}

fn parse_limit(rest: &mut Args, max: usize) -> Result<Option<usize>, String> {
    rest.value("--limit")?
        .map(|v| {
            let n = parse_u64("--limit", &v)?;
            usize::try_from(n)
                .ok()
                .filter(|n| (1..=max).contains(n))
                .ok_or_else(|| format!("--limit must be 1 to {max}, got {v:?}"))
        })
        .transpose()
}

fn parse_opt_id(rest: &mut Args, flag: &str, kind: IdKind) -> Result<Option<String>, String> {
    let value = rest.value(flag)?;
    if let Some(id) = &value {
        check_id(flag, kind, id)?;
    }
    Ok(value)
}

fn check_id(what: &str, kind: IdKind, id: &str) -> Result<(), String> {
    if valid_id(kind, id) {
        Ok(())
    } else {
        Err(format!(
            "{what} {id:?} is not {}<16 lowercase hex digits>",
            kind.prefix()
        ))
    }
}

fn parse_opt_token(rest: &mut Args, flag: &str) -> Result<Option<String>, String> {
    let value = rest.value(flag)?;
    if let Some(token) = &value
        && !valid_token(token)
    {
        return Err(format!(
            "{flag} must be 1 to {MAX_TOKEN_BYTES} bytes of A-Z a-z 0-9 . _ : / -"
        ));
    }
    Ok(value)
}

fn split_csv<'a>(flag: &str, raw: &'a str) -> Result<Vec<&'a str>, String> {
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return Err(format!("{flag} expects a non-empty value"));
    }
    Ok(parts)
}

fn parse_kind(s: &str) -> Result<PostKind, String> {
    match s {
        "summary" => Ok(PostKind::Summary),
        "milestone" => Ok(PostKind::Milestone),
        "blocker" => Ok(PostKind::Blocker),
        "decision" => Ok(PostKind::Decision),
        "handoff" => Ok(PostKind::Handoff),
        other => Err(format!(
            "unknown --kind {other:?} (expected summary, milestone, blocker, decision, handoff)"
        )),
    }
}

fn parse_outcome(s: &str) -> Result<Outcome, String> {
    match s {
        "succeeded" => Ok(Outcome::Succeeded),
        "partial" => Ok(Outcome::Partial),
        "failed" => Ok(Outcome::Failed),
        "cancelled" => Ok(Outcome::Cancelled),
        "unknown" => Ok(Outcome::Unknown),
        other => Err(format!(
            "unknown --outcome {other:?} (expected succeeded, partial, failed, cancelled, unknown)"
        )),
    }
}

// ---------------------------------------------------------------------------
// Execution

pub(crate) fn execute(
    client: &mut Client,
    own: Option<u64>,
    cmd: TimelineCommand,
) -> Result<i32, Failure> {
    let outbox_dir = noa_config::timeline_outbox_dir().map(|dir| dir.join("cli"));
    let result_env = std::env::var(super::result::RESULT_ENV)
        .ok()
        .filter(|s| !s.is_empty());
    let mut stdout = io::stdout().lock();
    execute_with(
        client,
        own,
        cmd,
        outbox_dir.as_deref(),
        result_env.as_deref(),
        &mut stdout,
    )
}

fn execute_with(
    api: &mut dyn TimelineApi,
    own: Option<u64>,
    cmd: TimelineCommand,
    outbox_dir: Option<&Path>,
    result_env: Option<&str>,
    out: &mut dyn Write,
) -> Result<i32, Failure> {
    if !api.supports() {
        return Err(unsupported_failure(result_env));
    }

    match cmd {
        TimelineCommand::Context { pane } => {
            let pane = pane.or(own).ok_or_else(|| {
                Failure::error(
                    "no --pane given and this process is not running inside a Noa pane \
                     (pass --pane PANE)",
                )
            })?;
            let ctx = api
                .context(&ContextParams {
                    board_id: None,
                    pane_id: Some(WireId(pane)),
                })
                .map_err(read_failure)?;
            emit_json(out, &ctx);
        }
        TimelineCommand::Boards { before, limit } => {
            let boards = api
                .boards(&BoardsParams { before, limit })
                .map_err(read_failure)?;
            emit_json(out, &boards);
        }
        TimelineCommand::Post(args) => {
            execute_post(api, own, args, outbox_dir, out)?;
        }
        TimelineCommand::List {
            board,
            before,
            limit,
            filter,
            query,
        } => {
            let board_id = resolve_board_id(api, own, board)?;
            let filter = if filter_is_empty(&filter) {
                None
            } else {
                Some(filter)
            };
            let page = api
                .list(&ListParams {
                    before_seq: before.map(Seq),
                    board_id,
                    filter,
                    limit,
                    query,
                })
                .map_err(read_failure)?;
            emit_json(out, &page);
        }
        TimelineCommand::Show { board, post } => {
            let board_id = resolve_board_id(api, own, board)?;
            let result = api
                .get(&GetParams {
                    board_id,
                    post_id: post,
                })
                .map_err(read_failure)?;
            emit_json(out, &result);
        }
        TimelineCommand::Changes {
            board,
            after,
            limit,
        } => {
            let board_id = resolve_board_id(api, own, board)?;
            let page = api
                .changes(&ChangesParams {
                    after_seq: Seq(after),
                    board_id,
                    limit,
                })
                .map_err(read_failure)?;
            emit_json(out, &page);
        }
        TimelineCommand::Export {
            board,
            format,
            output,
        } => {
            execute_export(api, own, board, format, &output, out)?;
        }
    }
    Ok(EXIT_OK)
}

fn unsupported_failure(result_env: Option<&str>) -> Failure {
    let mut message = format!(
        "this Noa does not support the board timeline (missing capability \
         {CAPABILITY_BOARD_TIMELINE_V1})"
    );
    if let Some(path) = result_env {
        let _ = write!(message, "; worker report path: {path} (NOA_RESULT)");
    }
    Failure::unavailable(message)
}

/// A failed read (`context`, `boards`, `list`, `get`, `changes`).
fn read_failure(error: ClientError) -> Failure {
    call_failure(error, None)
}

/// How a failed timeline call ends the command. Exit 3 here keeps the
/// meaning it has for every `+ctl` command (the connection to Noa was lost,
/// Noa is full of connections, the token lacks the scope), plus the missing
/// `boardTimelineV1` capability checked up front; a request Noa did not
/// confirm in time (`outcome_unknown`, or the client's own deadline) is an
/// ordinary failure, exit 1, not a server without the timeline.
/// `request_id`: the id a write was sent under, which a re-run resends.
fn call_failure(error: ClientError, request_id: Option<&str>) -> Failure {
    match unconfirmed(&error) {
        Some(what) => unconfirmed_failure(&what, request_id),
        None => Failure::from(error),
    }
}

/// What happened, when `error` leaves the request's outcome unknown: the
/// service queued it but did not answer within its 4 s (`outcome_unknown`,
/// -32009), or no answer arrived within the client's deadline.
fn unconfirmed(error: &ClientError) -> Option<String> {
    match error {
        ClientError::Rpc { code, message }
            if TimelineErrorKind::from_code(*code) == Some(TimelineErrorKind::OutcomeUnknown) =>
        {
            Some(format!(
                "Noa did not confirm the timeline request in time (outcome_unknown: {message})"
            ))
        }
        ClientError::Timeout(_) => Some(format!(
            "Noa did not answer the timeline request in time ({error})"
        )),
        _ => None,
    }
}

fn unconfirmed_failure(what: &str, request_id: Option<&str>) -> Failure {
    Failure::error(match request_id {
        Some(id) => format!(
            "{what}; requestId {id} may still have been saved: re-run the same command, which \
             resends it under the same requestId and prints the result"
        ),
        None => format!("{what}; it only reads, so running it again is safe"),
    })
}

fn emit_json(out: &mut dyn Write, value: &impl Serialize) {
    let json = serde_json::to_value(value).expect("timeline response serializes");
    if let Err(err) = writeln!(out, "{json}")
        && err.kind() != io::ErrorKind::BrokenPipe
    {
        panic!("failed printing to stdout: {err}");
    }
}

fn filter_is_empty(filter: &Filter) -> bool {
    filter.worker_ids.is_empty()
        && filter.agent_kinds.is_empty()
        && filter.kinds.is_empty()
        && filter.outcomes.is_empty()
}

fn resolve_board_id(
    api: &mut dyn TimelineApi,
    own: Option<u64>,
    board: Option<String>,
) -> Result<String, Failure> {
    if let Some(board_id) = board {
        return Ok(board_id);
    }
    let pane = own.ok_or_else(|| {
        Failure::error(
            "no --board given and this process is not running inside a Noa pane \
             (pass --board BOARD_ID)",
        )
    })?;
    let ctx = api
        .context(&ContextParams {
            board_id: None,
            pane_id: Some(WireId(pane)),
        })
        .map_err(read_failure)?;
    Ok(ctx.board_id)
}

// ---------------------------------------------------------------------------
// Post

fn execute_post(
    api: &mut dyn TimelineApi,
    own: Option<u64>,
    args: PostArgs,
    outbox_dir: Option<&Path>,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let (board_id, run_id, producer_id, post, take_over) = match args.source {
        PostSource::File(path) => {
            let post = read_post_file(&path)?;
            let (board_id, run_id, producer_id) =
                resolve_file_post_ids(api, own, args.board, args.run, args.producer, &post)?;
            (board_id, run_id, producer_id, post, false)
        }
        PostSource::FromResult {
            path,
            title,
            summary,
            outcome,
            kind,
            take_over,
        } => {
            let bytes = read_result_file(&path, MAX_BODY_BYTES).map_err(|err| {
                Failure::error(format!("cannot read result file {}: {err}", path.display()))
            })?;
            let body_markdown = String::from_utf8(bytes.clone()).map_err(|_| {
                Failure::error(format!("result file {} is not valid UTF-8", path.display()))
            })?;
            let (board_id, ctx) = resolve_board_context(api, own, args.board)?;
            let run_id = args.run;
            let producer_id = resolve_result_producer(
                api,
                &board_id,
                ctx.as_ref(),
                run_id.as_deref(),
                args.producer,
                take_over,
            )?;
            let post = build_result_post(
                ResultPostSpec {
                    path: &path,
                    raw_bytes: &bytes,
                    body_markdown,
                    title,
                    summary,
                    outcome,
                    kind,
                },
                run_id.as_deref(),
            )?;
            (board_id, run_id, producer_id, post, take_over)
        }
    };

    let request_id = args
        .request_id
        .unwrap_or_else(|| default_request_id(&board_id, run_id.as_deref(), &producer_id, &post));

    // Check wire payload budget before touching the outbox or mutating policy.
    let probe = PostParams {
        board_id: board_id.clone(),
        post: post.clone(),
        producer_id: producer_id.clone(),
        request_id: request_id.clone(),
        run_id: run_id.clone(),
    };
    let encoded_len = serde_json::to_vec(&probe)
        .expect("PostParams serializes")
        .len();
    if encoded_len > MAX_POST_PARAMS_BYTES {
        return Err(Failure::error(format!(
            "post payload is {encoded_len} bytes; the most is {MAX_POST_PARAMS_BYTES}"
        )));
    }

    let outbox_dir = outbox_dir
        .ok_or_else(|| Failure::error("cannot resolve timeline outbox directory"))?
        .to_path_buf();
    let outbox = Outbox::open(outbox_dir, false)
        .map_err(|err| Failure::error(format!("cannot open timeline outbox: {err}")))?;

    let envelope = Envelope {
        board_id,
        post,
        producer_id,
        request_id,
        run_id,
    };
    // Durable before anything reaches Noa, take-over included: a post the
    // outbox refuses (a conflict, a write error) leaves the run's policy as
    // it was. `publish` stages it again, which finds this same entry.
    if let Err(err) = outbox.stage(&envelope) {
        let kind =
            matches!(err, OutboxError::Conflict { .. }).then_some(TimelineErrorKind::Conflict);
        return Err(Failure::error(format_publish_error(kind, &err.to_string())));
    }

    if take_over {
        let target_run = envelope
            .run_id
            .as_deref()
            .expect("--take-over requires --run (checked by parser)");
        let policy_req_id = take_over_request_id(
            &envelope.board_id,
            target_run,
            &envelope.producer_id,
            &envelope.request_id,
        );
        api.set_publish_policy(&SetPublishPolicyParams {
            board_id: envelope.board_id.clone(),
            publish_policy: PublishPolicy {
                owner: PublishOwner::Manual,
                producer_id: Some(envelope.producer_id.clone()),
            },
            request_id: policy_req_id.clone(),
            run_id: target_run.to_string(),
        })
        .map_err(|err| call_failure(err, Some(&policy_req_id)))?;
    }

    struct CapturingSink<'a> {
        api: &'a mut dyn TimelineApi,
        ack: Option<PostResult>,
        /// Set when the send's outcome is unknown ([`unconfirmed`]).
        unconfirmed: Option<String>,
    }
    impl PostSink for CapturingSink<'_> {
        fn post(&mut self, p: &PostParams) -> Result<PostResult, ClientError> {
            let res = self.api.post(p).inspect_err(|err| {
                self.unconfirmed = unconfirmed(err);
            })?;
            self.ack = Some(res.clone());
            Ok(res)
        }
    }

    let request_id = envelope.request_id.clone();
    let mut sink = CapturingSink {
        api,
        ack: None,
        unconfirmed: None,
    };
    match publish(&mut sink, &outbox, envelope) {
        PublishState::Published { post_id, seq } => {
            if let Some(ack) = sink.ack {
                emit_json(out, &ack);
            } else {
                emit_json(
                    out,
                    &serde_json::json!({
                        "duplicate": true,
                        "postId": post_id,
                        "seq": seq,
                    }),
                );
            }
            Ok(())
        }
        // The envelope stays staged either way, so every failure here is
        // exit 1: re-running the command resends it.
        PublishState::Failed { kind, message } => Err(match sink.unconfirmed {
            Some(what) => unconfirmed_failure(&what, Some(&request_id)),
            None => Failure::error(format_publish_error(kind, &message)),
        }),
        PublishState::Pending => Err(Failure::error("post was not acknowledged")),
    }
}

fn format_publish_error(kind: Option<TimelineErrorKind>, message: &str) -> String {
    match kind {
        Some(kind) => format!("timeline post failed ({kind}): {message}"),
        None => format!("timeline post failed: {message}"),
    }
}

fn read_post_file(path: &Path) -> Result<PostInput, Failure> {
    // Bounded like `--from-result`: a FIFO is refused instead of blocking,
    // and nothing past the post budget is read.
    let bytes = read_result_file(path, MAX_POST_PARAMS_BYTES)
        .map_err(|err| Failure::error(format!("cannot read {}: {err}", path.display())))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|err| Failure::error(format!("{} is not valid JSON: {err}", path.display())))?;
    let object = value
        .as_object()
        .ok_or_else(|| Failure::error(format!("{} must contain a JSON object", path.display())))?;
    let forbidden: Vec<&str> = FORBIDDEN_FILE_KEYS
        .into_iter()
        .filter(|key| object.contains_key(*key))
        .collect();
    if !forbidden.is_empty() {
        return Err(Failure::error(format!(
            "{} must not specify {} (pass envelope ids via CLI flags or pane context)",
            path.display(),
            forbidden.join(", ")
        )));
    }
    if let Some(version) = object
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        && version != u64::from(SCHEMA_VERSION)
    {
        return Err(Failure::error(format!(
            "schemaVersion {version} in {} is not supported (this Noa takes {SCHEMA_VERSION})",
            path.display()
        )));
    }
    let post: PostInput = serde_json::from_value(value)
        .map_err(|err| Failure::error(format!("invalid post in {}: {err}", path.display())))?;
    let post = post.normalized();
    post.validate()
        .map_err(|err| Failure::error(err.to_string()))?;
    Ok(post)
}

fn resolve_board_context(
    api: &mut dyn TimelineApi,
    own: Option<u64>,
    board: Option<String>,
) -> Result<(String, Option<ContextResult>), Failure> {
    if let Some(board_id) = board {
        return Ok((board_id, None));
    }
    let pane = own.ok_or_else(|| {
        Failure::error(
            "no --board given and this process is not running inside a Noa pane \
             (pass --board BOARD_ID)",
        )
    })?;
    let ctx = api
        .context(&ContextParams {
            board_id: None,
            pane_id: Some(WireId(pane)),
        })
        .map_err(read_failure)?;
    let board_id = ctx.board_id.clone();
    Ok((board_id, Some(ctx)))
}

fn resolve_file_post_ids(
    api: &mut dyn TimelineApi,
    own: Option<u64>,
    board: Option<String>,
    run: Option<String>,
    producer: Option<String>,
    post: &PostInput,
) -> Result<(String, Option<String>, String), Failure> {
    let unregistered_ok = UNREGISTERED_NAMESPACES.contains(&post.source_ref.namespace.as_str());

    let mut ctx: Option<ContextResult> = None;
    let board_id = match (&board, own) {
        (Some(b), _) => b.clone(),
        (None, Some(pane)) => {
            let fetched = api
                .context(&ContextParams {
                    board_id: None,
                    pane_id: Some(WireId(pane)),
                })
                .map_err(read_failure)?;
            let id = fetched.board_id.clone();
            ctx = Some(fetched);
            id
        }
        (None, None) => {
            return Err(Failure::error(
                "no --board given and this process is not running inside a Noa pane \
                 (pass --board BOARD_ID)",
            ));
        }
    };

    let run_id = match run {
        Some(r) => Some(r),
        None => {
            if let Some(pane) = own {
                if ctx.is_none() {
                    // Only with --board (without it the context was fetched
                    // above). A pane on no board posts as one outside a
                    // pane does, when the namespace allows no run.
                    match api.context(&ContextParams {
                        board_id: None,
                        pane_id: Some(WireId(pane)),
                    }) {
                        Ok(fetched) => ctx = Some(fetched),
                        Err(ClientError::Rpc { code, .. })
                            if unregistered_ok
                                && TimelineErrorKind::from_code(code)
                                    == Some(TimelineErrorKind::NotFound) => {}
                        Err(err) => return Err(read_failure(err)),
                    }
                }
                if let Some(pane_ctx) = ctx.as_ref().filter(|c| c.board_id == board_id) {
                    let runs = pane_ctx
                        .me
                        .as_ref()
                        .map(|me| me.runs.as_slice())
                        .unwrap_or(&[]);
                    if runs.is_empty()
                        && unregistered_ok
                        && pane_ctx.me.as_ref().is_none_or(|m| m.role != "worker")
                    {
                        None
                    } else {
                        Some(
                            super::run_attr::choose_run(runs, None)
                                .map_err(Failure::error)?
                                .to_string(),
                        )
                    }
                } else if unregistered_ok {
                    // The pane is on another board, or on none.
                    None
                } else {
                    let pane_board = ctx.as_ref().expect("refused above without one");
                    return Err(Failure::error(format!(
                        "--board {board_id} differs from this pane's board {}; pass --run",
                        pane_board.board_id
                    )));
                }
            } else if unregistered_ok {
                None
            } else {
                return Err(Failure::error(
                    "no registered run (pass --run, or --from-result with --take-over from the \
                     orchestrator)",
                ));
            }
        }
    };

    let producer_id = match producer {
        Some(p) => p,
        None => match run_id.as_deref() {
            Some(run_id) => {
                let board_ctx = match ctx {
                    Some(c) if c.board_id == board_id => c,
                    _ => api
                        .context(&ContextParams {
                            board_id: Some(board_id.clone()),
                            pane_id: None,
                        })
                        .map_err(read_failure)?,
                };
                producer_for_run(&board_ctx, run_id).ok_or_else(|| {
                    Failure::error(format!(
                        "cannot determine producerId for run {run_id} on board {board_id} \
                         (pass --producer ID)"
                    ))
                })?
            }
            None => DEFAULT_MANUAL_PRODUCER.to_string(),
        },
    };

    Ok((board_id, run_id, producer_id))
}

fn resolve_result_producer(
    api: &mut dyn TimelineApi,
    board_id: &str,
    ctx: Option<&ContextResult>,
    run_id: Option<&str>,
    producer: Option<String>,
    take_over: bool,
) -> Result<String, Failure> {
    if let Some(p) = producer {
        return Ok(p);
    }
    if take_over || run_id.is_none() {
        return Ok(DEFAULT_MANUAL_PRODUCER.to_string());
    }
    let run_id = run_id.expect("checked Some above");
    if let Some(ctx) = ctx
        && ctx.board_id == board_id
        && let Some(p) = producer_for_run(ctx, run_id)
    {
        return Ok(p);
    }
    let fetched = api
        .context(&ContextParams {
            board_id: Some(board_id.to_string()),
            pane_id: None,
        })
        .map_err(read_failure)?;
    // An unknown producer is refused, as `--file` refuses it, instead of
    // being guessed as `cli:manual`.
    producer_for_run(&fetched, run_id).ok_or_else(|| {
        Failure::error(format!(
            "cannot determine producerId for run {run_id} on board {board_id} \
             (pass --producer ID)"
        ))
    })
}

fn producer_for_run(ctx: &ContextResult, run_id: &str) -> Option<String> {
    let run = ctx
        .me
        .as_ref()
        .into_iter()
        .flat_map(|me| me.runs.iter())
        .chain(ctx.workers.iter().flat_map(|w| w.runs.iter()))
        .find(|run| run.run_id == run_id)?;
    if let Some(producer) = &run.publish_policy.producer_id {
        return Some(producer.clone());
    }
    if run.publish_policy.owner == PublishOwner::Worker {
        return run.worker_id.clone();
    }
    None
}

struct ResultPostSpec<'a> {
    path: &'a Path,
    raw_bytes: &'a [u8],
    body_markdown: String,
    title: String,
    summary: String,
    outcome: Outcome,
    kind: PostKind,
}

fn build_result_post(spec: ResultPostSpec<'_>, run_id: Option<&str>) -> Result<PostInput, Failure> {
    let path_str = spec.path.to_string_lossy().into_owned();
    let event_id = format!(
        "file-{:016x}-{:016x}",
        fnv1a64(&[path_str.as_bytes()]),
        fnv1a64(&[spec.raw_bytes]),
    );
    let label = artifact_label(spec.path);
    let post = PostInput {
        artifacts: vec![Artifact {
            digest: None,
            kind: "file".to_string(),
            label,
            r#ref: path_str,
        }],
        author: None,
        based_on_post_ids: Vec::new(),
        body_markdown: spec.body_markdown,
        checks: Vec::new(),
        kind: spec.kind,
        next_actions: Vec::new(),
        occurred_at: None,
        open_issues: Vec::new(),
        outcome: spec.outcome,
        retry_of_run_id: None,
        schema_version: SCHEMA_VERSION,
        source_ref: SourceRef {
            event_id,
            locator: None,
            namespace: "result-file".to_string(),
            run_id: None,
            session_id: run_id.unwrap_or("none").to_string(),
        },
        summary: spec.summary,
        supersedes_post_id: None,
        task_ref: None,
        title: spec.title,
    }
    .normalized();
    post.validate()
        .map_err(|err| Failure::error(err.to_string()))?;
    Ok(post)
}

fn artifact_label(path: &Path) -> String {
    let raw = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "result".to_string());
    if raw.len() <= MAX_TOKEN_BYTES {
        return raw;
    }
    let mut end = MAX_TOKEN_BYTES;
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    raw[..end].to_string()
}

/// Deterministic CLI request id: `cli-<fnv1a64(boardId, runId, producerId, canonical PostInput)>`.
pub(crate) fn default_request_id(
    board_id: &str,
    run_id: Option<&str>,
    producer_id: &str,
    post: &PostInput,
) -> String {
    let canon = canon_bytes(run_id, Some(producer_id), post);
    let hash = fnv1a64(&[
        board_id.as_bytes(),
        b"\0",
        run_id.unwrap_or("").as_bytes(),
        b"\0",
        producer_id.as_bytes(),
        b"\0",
        &canon,
    ]);
    format!("cli-{hash:016x}")
}

/// The `setPublishPolicy` requestId of a `--take-over`: derived from the
/// post's own requestId, so re-running the same command replays its policy
/// change while a later take-over (another post) records a new one, even by
/// a producer that held the run before.
fn take_over_request_id(
    board_id: &str,
    run_id: &str,
    producer_id: &str,
    post_request_id: &str,
) -> String {
    let hash = fnv1a64(&[
        b"take-over\0",
        board_id.as_bytes(),
        b"\0",
        run_id.as_bytes(),
        b"\0",
        producer_id.as_bytes(),
        b"\0",
        post_request_id.as_bytes(),
    ]);
    format!("cli-policy-{hash:016x}")
}

fn fnv1a64(chunks: &[&[u8]]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for chunk in chunks {
        for &byte in *chunk {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3_u64);
        }
    }
    hash
}

// ---------------------------------------------------------------------------
// Result file reading

/// Reads a regular file up to `cap` bytes without following symlinks or
/// blocking on a FIFO (`open(O_RDONLY|O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC)` +
/// `fstat` `S_ISREG`). Any file larger than `cap` is refused, never truncated.
pub(crate) fn read_result_file(path: &Path, cap: usize) -> io::Result<Vec<u8>> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let limit = (cap as u64).saturating_add(1);
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} exceeds the {cap}-byte limit", path.display()),
        ));
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Export

fn execute_export(
    api: &mut dyn TimelineApi,
    own: Option<u64>,
    board: Option<String>,
    format: ExportFormat,
    output: &Path,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let board_ctx = match board {
        Some(board_id) => api
            .context(&ContextParams {
                board_id: Some(board_id),
                pane_id: None,
            })
            .map_err(read_failure)?,
        None => {
            let pane = own.ok_or_else(|| {
                Failure::error(
                    "no --board given and this process is not running inside a Noa pane \
                     (pass --board BOARD_ID)",
                )
            })?;
            api.context(&ContextParams {
                board_id: None,
                pane_id: Some(WireId(pane)),
            })
            .map_err(read_failure)?
        }
    };

    let mut after = Seq(0);
    let mut summaries = Vec::new();
    let head_seq = loop {
        let page = api
            .changes(&ChangesParams {
                after_seq: after,
                board_id: board_ctx.board_id.clone(),
                limit: Some(CHANGES_MAX),
            })
            .map_err(read_failure)?;
        let last_seq = page.posts.last().map(|p| p.seq);
        summaries.extend(page.posts);
        if !page.has_more {
            break page.head_seq;
        }
        match page.next_after_seq.or(last_seq) {
            Some(next) if next > after => after = next,
            _ => {
                return Err(Failure::error(
                    "timeline changes pagination did not advance while hasMore was true",
                ));
            }
        }
    };

    let mut posts = Vec::with_capacity(summaries.len());
    for summary in &summaries {
        let fetched = api
            .get(&GetParams {
                board_id: board_ctx.board_id.clone(),
                post_id: summary.post_id.clone(),
            })
            .map_err(read_failure)?;
        posts.push(fetched.post);
    }

    let mut export_board = board_ctx;
    export_board.head_seq = head_seq;

    let rendered = match format {
        ExportFormat::Markdown => export_markdown(&export_board, &posts),
        ExportFormat::Json => export_json(&export_board, &posts),
    };

    crate::atomic_write::write_atomic(output, rendered.as_bytes())
        .map_err(|err| Failure::error(format!("cannot write {}: {err}", output.display())))?;

    emit_json(
        out,
        &serde_json::json!({
            "boardId": export_board.board_id,
            "format": match format {
                ExportFormat::Markdown => "markdown",
                ExportFormat::Json => "json",
            },
            "headSeq": export_board.head_seq,
            "output": output.to_string_lossy(),
            "posts": posts.len(),
        }),
    );
    Ok(())
}

pub(crate) fn export_markdown(board: &ContextResult, posts: &[Post]) -> String {
    let mut out = String::new();
    let heading = if board.name.trim().is_empty() {
        board.board_id.as_str()
    } else {
        board.name.as_str()
    };
    let _ = writeln!(out, "# {heading}\n");
    let _ = writeln!(out, "- Board ID: `{}`", board.board_id);
    let _ = writeln!(out, "- Head Seq: `{}`", board.head_seq.0);
    let _ = writeln!(out, "- Archived: `{}`", board.archived);
    let _ = writeln!(out, "- Posts: `{}`", posts.len());

    for post in posts {
        let _ = writeln!(out, "\n---\n");
        let _ = writeln!(out, "## #{} {}\n", post.seq.0, post.input.title);
        let _ = writeln!(out, "- Board ID: `{}`", post.board_id);
        let _ = writeln!(out, "- Post ID: `{}`", post.post_id);
        let _ = writeln!(out, "- Seq: `{}`", post.seq.0);
        let _ = writeln!(out, "- Created At: `{}`", post.created_at);
        if let Some(at) = &post.input.occurred_at {
            let _ = writeln!(out, "- Occurred At: `{at}`");
        }
        let _ = writeln!(out, "- Kind: `{}`", post_kind_str(post.input.kind));
        let _ = writeln!(out, "- Outcome: `{}`", outcome_str(post.input.outcome));
        let _ = writeln!(out, "- Provenance: `{}`", provenance_str(post.provenance));
        let _ = writeln!(
            out,
            "- Producer: `{}` (`{}`)",
            post.producer.id,
            producer_kind_str(post.producer.kind)
        );
        if let Some(author) = format_author(post) {
            let _ = writeln!(out, "- Author: {author}");
        }
        if let Some(worker) = &post.worker_id {
            let _ = writeln!(out, "- Worker ID: `{worker}`");
        }
        if let Some(run) = &post.run_id {
            let _ = writeln!(out, "- Run ID: `{run}`");
        }
        if let Some(retry_of) = &post.input.retry_of_run_id {
            let _ = writeln!(out, "- Retry Of Run: `{retry_of}`");
        }
        if let Some(task) = &post.input.task_ref {
            let _ = write!(out, "- Task: `{}:{}`", task.kind, task.id);
            if let Some(title) = &task.title {
                let _ = write!(out, " {title}");
            }
            if let Some(url) = &task.url {
                let _ = write!(out, " ({url})");
            }
            out.push('\n');
        }
        if let Some(supersedes) = &post.input.supersedes_post_id {
            let _ = writeln!(out, "- Supersedes: `{supersedes}`");
        }
        if !post.input.based_on_post_ids.is_empty() {
            let refs: Vec<String> = post
                .input
                .based_on_post_ids
                .iter()
                .map(|id| format!("`{id}`"))
                .collect();
            let _ = writeln!(out, "- Based On: {}", refs.join(", "));
        }
        let src = &post.input.source_ref;
        let _ = write!(
            out,
            "- Source Ref: namespace=`{}` sessionId=`{}`",
            src.namespace, src.session_id
        );
        if let Some(run) = &src.run_id {
            let _ = write!(out, " runId=`{run}`");
        }
        let _ = write!(out, " eventId=`{}`", src.event_id);
        if let Some(locator) = &src.locator {
            let _ = write!(out, " locator=`{locator}`");
        }
        out.push('\n');

        let _ = writeln!(out, "\n### Summary\n\n{}", post.input.summary);

        if !post.input.body_markdown.is_empty() {
            let _ = writeln!(out, "\n### Body\n\n{}", post.input.body_markdown);
        }
        if !post.input.checks.is_empty() {
            let _ = writeln!(out, "\n### Checks\n");
            for check in &post.input.checks {
                let _ = write!(
                    out,
                    "- [{}] {} (reported by `{}`",
                    check_result_str(check.result),
                    check.name,
                    check.reported_by
                );
                if let Some(evidence) = &check.evidence_ref {
                    let _ = write!(out, ", evidence: `{evidence}`");
                }
                let _ = writeln!(out, ")");
            }
        }
        if !post.input.artifacts.is_empty() {
            let _ = writeln!(out, "\n### Artifacts\n");
            for artifact in &post.input.artifacts {
                let _ = write!(
                    out,
                    "- [{}] {}: `{}`",
                    artifact.kind, artifact.label, artifact.r#ref
                );
                if let Some(digest) = &artifact.digest {
                    let _ = write!(out, " (digest: `{digest}`)");
                }
                out.push('\n');
            }
        }
        if !post.input.open_issues.is_empty() {
            let _ = writeln!(out, "\n### Open Issues\n");
            for item in &post.input.open_issues {
                let _ = write!(out, "- {}", item.text);
                if let Some(r) = &item.r#ref {
                    let _ = write!(out, " (`{r}`)");
                }
                out.push('\n');
            }
        }
        if !post.input.next_actions.is_empty() {
            let _ = writeln!(out, "\n### Next Actions\n");
            for item in &post.input.next_actions {
                let _ = write!(out, "- {}", item.text);
                if let Some(r) = &item.r#ref {
                    let _ = write!(out, " (`{r}`)");
                }
                out.push('\n');
            }
        }
    }

    out
}

pub(crate) fn export_json(board: &ContextResult, posts: &[Post]) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct JsonExport<'a> {
        archived: bool,
        board_id: &'a str,
        head_seq: Seq,
        name: &'a str,
        posts: &'a [Post],
        schema_version: u32,
    }

    let doc = JsonExport {
        archived: board.archived,
        board_id: &board.board_id,
        head_seq: board.head_seq,
        name: &board.name,
        posts,
        schema_version: SCHEMA_VERSION,
    };
    let mut text = serde_json::to_string_pretty(&doc).expect("JsonExport serializes");
    text.push('\n');
    text
}

fn format_author(post: &Post) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(label) = &post.author.label {
        parts.push(label.clone());
    }
    if let Some(agent) = &post.author.agent_kind {
        parts.push(format!("agent=`{agent}`"));
    }
    if let Some(role) = &post.author.role {
        parts.push(format!("role=`{role}`"));
    }
    if let Some(worker) = &post.author.worker_id {
        parts.push(format!("worker=`{worker}`"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn post_kind_str(kind: PostKind) -> &'static str {
    match kind {
        PostKind::Summary => "summary",
        PostKind::Milestone => "milestone",
        PostKind::Blocker => "blocker",
        PostKind::Decision => "decision",
        PostKind::Handoff => "handoff",
    }
}

fn outcome_str(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Succeeded => "succeeded",
        Outcome::Partial => "partial",
        Outcome::Failed => "failed",
        Outcome::Cancelled => "cancelled",
        Outcome::Unknown => "unknown",
    }
}

fn provenance_str(provenance: Provenance) -> &'static str {
    match provenance {
        Provenance::Registered => "registered",
        Provenance::Unverified => "unverified",
    }
}

fn producer_kind_str(kind: ProducerKind) -> &'static str {
    match kind {
        ProducerKind::Worker => "worker",
        ProducerKind::Runner => "runner",
        ProducerKind::Human => "human",
    }
}

fn check_result_str(result: CheckResult) -> &'static str {
    match result {
        CheckResult::Passed => "passed",
        CheckResult::Failed => "failed",
        CheckResult::NotRun => "not_run",
        CheckResult::Inconclusive => "inconclusive",
    }
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::time::{Duration, Instant};

    use noa_ipc::timeline::{
        Author, BoardInfo, Check, CheckCounts, ContextSelf, ContextWorker, Item, PostSummary,
        Producer, TaskRef,
    };
    use serde_json::json;

    use super::*;
    use crate::ctl::{EXIT_ERROR, EXIT_UNAVAILABLE};

    const BOARD: &str = "bd-0123456789abcdef";
    const WORKER: &str = "wk-0123456789abcdef";
    const RUN_1: &str = "rn-0123456789abcdef";
    const RUN_2: &str = "rn-fedcba9876543210";
    const POST_1: &str = "po-0000000000000001";
    const POST_2: &str = "po-0000000000000002";

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "noa-ctl-timeline-{tag}-{:016x}",
                rand::random::<u64>()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample_run(run_id: &str) -> RunInfo {
        RunInfo {
            board_id: BOARD.to_string(),
            created_at: "2026-11-26T00:00:00Z".to_string(),
            publish_policy: PublishPolicy {
                owner: PublishOwner::Worker,
                producer_id: None,
            },
            retry_of_run_id: None,
            run_id: run_id.to_string(),
            summary_post_id: None,
            task_ref: None,
            worker_id: Some(WORKER.to_string()),
        }
    }

    fn sample_context(runs: Vec<RunInfo>) -> ContextResult {
        ContextResult {
            archived: false,
            board_id: BOARD.to_string(),
            head_seq: Seq(2),
            me: Some(ContextSelf {
                pane_id: WireId(7),
                role: "worker".to_string(),
                runs: runs.clone(),
                worker_id: Some(WORKER.to_string()),
            }),
            name: "Feature Board".to_string(),
            server_instance_id: "srv-1".to_string(),
            workers: vec![ContextWorker {
                agent_kind: Some("claude".to_string()),
                label: Some("ctl-cli".to_string()),
                live: true,
                pane_id: Some(WireId(7)),
                runs,
                surface_id: Some("surf-7".to_string()),
                worker_id: WORKER.to_string(),
            }],
        }
    }

    fn sample_post_json() -> serde_json::Value {
        json!({
            "schemaVersion": 1,
            "kind": "summary",
            "outcome": "partial",
            "title": "入力検証を追加",
            "summary": "境界値のテストを追加。GUI確認は未実施。",
            "bodyMarkdown": "## 実施内容\n入力値の境界チェックを追加した。",
            "sourceRef": {
                "namespace": "worker",
                "sessionId": "s1",
                "runId": "r1",
                "eventId": "ev-1"
            },
            "checks": [
                {
                    "name": "単体テスト",
                    "result": "passed",
                    "reportedBy": "worker"
                },
                {
                    "name": "GUI確認",
                    "result": "not_run",
                    "reportedBy": "worker"
                }
            ],
            "openIssues": ["GUIでの動作確認"],
            "nextActions": [{ "text": "対象画面で境界値を確認する", "ref": "docs/check.md" }]
        })
    }

    fn sample_post(post_id: &str, seq: u64, title: &str) -> Post {
        Post {
            author: Author {
                agent_kind: Some("claude".to_string()),
                label: Some("ctl-cli".to_string()),
                role: Some("implementer".to_string()),
                worker_id: Some(WORKER.to_string()),
            },
            board_id: BOARD.to_string(),
            created_at: format!("2026-11-26T00:0{seq}:00Z"),
            post_id: post_id.to_string(),
            producer: Producer {
                id: WORKER.to_string(),
                kind: ProducerKind::Worker,
            },
            provenance: Provenance::Registered,
            request_id: format!("cli-req-{seq}"),
            run_id: Some(RUN_1.to_string()),
            schema_version: SCHEMA_VERSION,
            seq: Seq(seq),
            worker_id: Some(WORKER.to_string()),
            input: PostInput {
                artifacts: vec![Artifact {
                    digest: Some("sha256:abc".to_string()),
                    kind: "file".to_string(),
                    label: "secret-should-not-inline.txt".to_string(),
                    r#ref: "/nonexistent/secret-should-not-inline.txt".to_string(),
                }],
                author: None,
                based_on_post_ids: Vec::new(),
                body_markdown: format!("Body for {post_id}."),
                checks: vec![Check {
                    evidence_ref: Some("ci/1".to_string()),
                    name: "unit".to_string(),
                    reported_by: "worker".to_string(),
                    result: CheckResult::Passed,
                }],
                kind: PostKind::Summary,
                next_actions: vec![Item {
                    r#ref: None,
                    text: "ship it".to_string(),
                }],
                occurred_at: Some(format!("2026-11-26T00:0{seq}:00Z")),
                open_issues: vec![Item {
                    r#ref: Some("issue-1".to_string()),
                    text: "check gui".to_string(),
                }],
                outcome: Outcome::Partial,
                retry_of_run_id: None,
                schema_version: SCHEMA_VERSION,
                source_ref: SourceRef {
                    event_id: format!("ev-{seq}"),
                    locator: Some("line:1".to_string()),
                    namespace: "worker".to_string(),
                    run_id: Some("r1".to_string()),
                    session_id: "s1".to_string(),
                },
                summary: format!("Summary for {post_id}."),
                supersedes_post_id: None,
                task_ref: Some(TaskRef {
                    id: "42".to_string(),
                    kind: "github".to_string(),
                    title: Some("Add timeline".to_string()),
                    url: Some("https://example.test/42".to_string()),
                }),
                title: title.to_string(),
            },
        }
    }

    fn summary_of(post: &Post) -> PostSummary {
        PostSummary {
            artifacts: post.input.artifacts.len() as u32,
            author: post.author.clone(),
            checks: CheckCounts {
                passed: 1,
                ..CheckCounts::default()
            },
            created_at: post.created_at.clone(),
            kind: post.input.kind,
            occurred_at: post.input.occurred_at.clone(),
            open_issues: post.input.open_issues.len() as u32,
            outcome: post.input.outcome,
            post_id: post.post_id.clone(),
            producer: post.producer.clone(),
            provenance: post.provenance,
            run_id: post.run_id.clone(),
            seq: post.seq,
            source_namespace: post.input.source_ref.namespace.clone(),
            summary: post.input.summary.clone(),
            superseded_by: Vec::new(),
            supersedes_post_id: post.input.supersedes_post_id.clone(),
            task_ref: post.input.task_ref.clone(),
            title: post.input.title.clone(),
            worker_id: post.worker_id.clone(),
        }
    }

    type PostHook = Box<dyn FnMut(&PostParams) -> Result<PostResult, ClientError>>;

    struct FakeApi {
        supported: bool,
        context_resp: Option<ContextResult>,
        boards_resp: Option<BoardsResult>,
        list_resp: Option<PageResult>,
        changes_resps: VecDeque<Result<PageResult, ClientError>>,
        get_resps: VecDeque<Result<GetResult, ClientError>>,
        policy_resps: VecDeque<Result<RunInfo, ClientError>>,
        post_hook: Option<PostHook>,
        /// Returned, in order, by the next `context`/`boards`/`list` calls.
        read_errors: VecDeque<ClientError>,
        calls: Vec<String>,
        posted: Vec<PostParams>,
        policy_calls: Vec<SetPublishPolicyParams>,
        boards_calls: Vec<BoardsParams>,
    }

    impl FakeApi {
        fn new() -> Self {
            Self {
                supported: true,
                context_resp: Some(sample_context(vec![sample_run(RUN_1)])),
                boards_resp: None,
                list_resp: None,
                changes_resps: VecDeque::new(),
                get_resps: VecDeque::new(),
                policy_resps: VecDeque::new(),
                post_hook: None,
                read_errors: VecDeque::new(),
                calls: Vec::new(),
                posted: Vec::new(),
                policy_calls: Vec::new(),
                boards_calls: Vec::new(),
            }
        }
    }

    impl TimelineApi for FakeApi {
        fn supports(&self) -> bool {
            self.supported
        }

        fn context(&mut self, p: &ContextParams) -> Result<ContextResult, ClientError> {
            self.calls.push(format!("context:{p:?}"));
            if let Some(err) = self.read_errors.pop_front() {
                return Err(err);
            }
            Ok(self
                .context_resp
                .clone()
                .expect("unexpected context call in test"))
        }

        fn boards(&mut self, p: &BoardsParams) -> Result<BoardsResult, ClientError> {
            self.calls.push(format!("boards:{p:?}"));
            self.boards_calls.push(p.clone());
            if let Some(err) = self.read_errors.pop_front() {
                return Err(err);
            }
            Ok(self
                .boards_resp
                .clone()
                .expect("unexpected boards call in test"))
        }

        fn set_publish_policy(
            &mut self,
            p: &SetPublishPolicyParams,
        ) -> Result<RunInfo, ClientError> {
            self.calls.push("setPublishPolicy".to_string());
            self.policy_calls.push(p.clone());
            self.policy_resps
                .pop_front()
                .expect("unexpected setPublishPolicy call in test")
        }

        fn post(&mut self, p: &PostParams) -> Result<PostResult, ClientError> {
            self.calls.push("post".to_string());
            self.posted.push(p.clone());
            (self
                .post_hook
                .as_mut()
                .expect("unexpected post call in test"))(p)
        }

        fn list(&mut self, p: &ListParams) -> Result<PageResult, ClientError> {
            self.calls.push(format!("list:{p:?}"));
            if let Some(err) = self.read_errors.pop_front() {
                return Err(err);
            }
            Ok(self
                .list_resp
                .clone()
                .expect("unexpected list call in test"))
        }

        fn get(&mut self, p: &GetParams) -> Result<GetResult, ClientError> {
            self.calls.push(format!("get:{}", p.post_id));
            self.get_resps
                .pop_front()
                .expect("unexpected get call in test")
        }

        fn changes(&mut self, p: &ChangesParams) -> Result<PageResult, ClientError> {
            self.calls.push(format!("changes:{}", p.after_seq.0));
            self.changes_resps
                .pop_front()
                .expect("unexpected changes call in test")
        }
    }

    // -----------------------------------------------------------------------
    // AC-2 parse tests

    #[test]
    fn ac2_parse_context() {
        assert_eq!(
            parse(&strings(&["context"])).unwrap(),
            TimelineCommand::Context { pane: None }
        );
        assert_eq!(
            parse(&strings(&["context", "--pane", "42"])).unwrap(),
            TimelineCommand::Context { pane: Some(42) }
        );
        assert!(parse(&strings(&["context", "--pane", "abc"])).is_err());
        assert!(parse(&strings(&["context", "extra"])).is_err());
        assert!(parse(&strings(&[])).is_err());
        assert!(parse(&strings(&["bogus"])).is_err());
    }

    #[test]
    fn ac2_parse_boards() {
        assert_eq!(
            parse(&strings(&["boards"])).unwrap(),
            TimelineCommand::Boards {
                before: None,
                limit: None
            }
        );
        assert_eq!(
            parse(&strings(&["boards", "--limit", "10"])).unwrap(),
            TimelineCommand::Boards {
                before: None,
                limit: Some(10)
            }
        );
        assert!(parse(&strings(&["boards", "--limit", "0"])).is_err());
        assert!(parse(&strings(&["boards", "--limit", "201"])).is_err());
    }

    #[test]
    fn ac2_parse_post() {
        assert_eq!(
            parse(&strings(&[
                "post",
                "--board",
                BOARD,
                "--run",
                RUN_1,
                "--request-id",
                "req-1",
                "--producer",
                WORKER,
                "--file",
                "summary.json",
            ]))
            .unwrap(),
            TimelineCommand::Post(PostArgs {
                board: Some(BOARD.to_string()),
                run: Some(RUN_1.to_string()),
                producer: Some(WORKER.to_string()),
                request_id: Some("req-1".to_string()),
                source: PostSource::File(PathBuf::from("summary.json")),
            })
        );
        assert_eq!(
            parse(&strings(&[
                "post",
                "--board",
                BOARD,
                "--run",
                RUN_1,
                "--from-result",
                "/tmp/r.md",
                "--title",
                "Done",
                "--summary",
                "All tests pass.",
                "--outcome",
                "partial",
                "--kind",
                "handoff",
                "--take-over",
            ]))
            .unwrap(),
            TimelineCommand::Post(PostArgs {
                board: Some(BOARD.to_string()),
                run: Some(RUN_1.to_string()),
                producer: None,
                request_id: None,
                source: PostSource::FromResult {
                    path: PathBuf::from("/tmp/r.md"),
                    title: "Done".to_string(),
                    summary: "All tests pass.".to_string(),
                    outcome: Outcome::Partial,
                    kind: PostKind::Handoff,
                    take_over: true,
                },
            })
        );
        for bad in [
            &["post"][..],
            &["post", "--file", "a.json", "--from-result", "r.md"],
            &["post", "--file", "a.json", "--title", "t"],
            &["post", "--file", "a.json", "--take-over", "--run", RUN_1],
            &["post", "--from-result", "r.md", "--title", "t"],
            &[
                "post",
                "--from-result",
                "r.md",
                "--title",
                "t",
                "--summary",
                "s",
                "--outcome",
                "bogus",
            ],
            &[
                "post",
                "--from-result",
                "r.md",
                "--title",
                "t",
                "--summary",
                "s",
                "--outcome",
                "succeeded",
                "--take-over",
            ],
            &["post", "--board", "../bad", "--file", "a.json"],
            &["post", "--run", "not-a-run", "--file", "a.json"],
            &["post", "--request-id", "has space", "--file", "a.json"],
        ] {
            assert!(parse(&strings(bad)).is_err(), "{bad:?} must fail");
        }
    }

    #[test]
    fn ac2_parse_list() {
        assert_eq!(
            parse(&strings(&[
                "list",
                "--board",
                BOARD,
                "--before",
                "50",
                "--limit",
                "20",
                "--worker",
                WORKER,
                "--agent",
                "claude,codex",
                "--kind",
                "summary",
                "--kind",
                "decision",
                "--outcome",
                "succeeded,partial",
                "--query",
                "auth",
            ]))
            .unwrap(),
            TimelineCommand::List {
                board: Some(BOARD.to_string()),
                before: Some(50),
                limit: Some(20),
                filter: Filter {
                    agent_kinds: strings(&["claude", "codex"]),
                    kinds: vec![PostKind::Summary, PostKind::Decision],
                    outcomes: vec![Outcome::Succeeded, Outcome::Partial],
                    worker_ids: vec![WORKER.to_string()],
                },
                query: Some("auth".to_string()),
            }
        );
        assert!(parse(&strings(&["list", "--limit", "51"])).is_err());
        assert!(parse(&strings(&["list", "--worker", "bad"])).is_err());
        assert!(parse(&strings(&["list", "--before", "1", "--before-seq", "2"])).is_err());
    }

    #[test]
    fn ac2_parse_show() {
        assert_eq!(
            parse(&strings(&["show", "--board", BOARD, POST_1])).unwrap(),
            TimelineCommand::Show {
                board: Some(BOARD.to_string()),
                post: POST_1.to_string(),
            }
        );
        assert_eq!(
            parse(&strings(&["show", "--board", BOARD, "--post", POST_1])).unwrap(),
            TimelineCommand::Show {
                board: Some(BOARD.to_string()),
                post: POST_1.to_string(),
            }
        );
        assert!(parse(&strings(&["show"])).is_err());
        assert!(parse(&strings(&["show", "--post", POST_1, POST_2])).is_err());
        assert!(parse(&strings(&["show", "not-a-post"])).is_err());
    }

    #[test]
    fn ac2_parse_changes() {
        assert_eq!(
            parse(&strings(&["changes"])).unwrap(),
            TimelineCommand::Changes {
                board: None,
                after: 0,
                limit: None,
            }
        );
        assert_eq!(
            parse(&strings(&[
                "changes",
                "--board",
                BOARD,
                "--after-seq",
                "42",
                "--limit",
                "100",
            ]))
            .unwrap(),
            TimelineCommand::Changes {
                board: Some(BOARD.to_string()),
                after: 42,
                limit: Some(100),
            }
        );
        assert_eq!(
            parse(&strings(&["changes", "--after", "9"])).unwrap(),
            TimelineCommand::Changes {
                board: None,
                after: 9,
                limit: None,
            }
        );
        assert!(parse(&strings(&["changes", "--after", "1", "--after-seq", "2"])).is_err());
        assert!(parse(&strings(&["changes", "--limit", "201"])).is_err());
    }

    #[test]
    fn ac2_parse_export() {
        assert_eq!(
            parse(&strings(&[
                "export", "--board", BOARD, "--format", "markdown", "--output", "board.md",
            ]))
            .unwrap(),
            TimelineCommand::Export {
                board: Some(BOARD.to_string()),
                format: ExportFormat::Markdown,
                output: PathBuf::from("board.md"),
            }
        );
        assert_eq!(
            parse(&strings(&[
                "export",
                "--format",
                "json",
                "--output",
                "board.json",
            ]))
            .unwrap(),
            TimelineCommand::Export {
                board: None,
                format: ExportFormat::Json,
                output: PathBuf::from("board.json"),
            }
        );
        assert!(parse(&strings(&["export", "--output", "a.md"])).is_err());
        assert!(parse(&strings(&["export", "--format", "yaml", "--output", "a"])).is_err());
        assert!(parse(&strings(&["export", "--format", "json"])).is_err());
    }

    // -----------------------------------------------------------------------
    // AC-2 result file safety tests

    #[cfg(unix)]
    #[test]
    fn ac2_fifo_refused_without_hanging() {
        let dir = TempDir::new("fifo");
        let fifo = dir.0.join("pipe");
        let c_path = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: mkfifo reads the NUL-terminated path only.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        let err = read_result_file(&fifo, MAX_BODY_BYTES).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "reading a FIFO must not hang"
        );
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        // Directories are also not regular files.
        assert_eq!(
            read_result_file(&dir.0, MAX_BODY_BYTES).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[cfg(unix)]
    #[test]
    fn ac2_symlink_refused() {
        let dir = TempDir::new("symlink");
        let target = dir.0.join("real.md");
        let link = dir.0.join("link.md");
        fs::write(&target, "real content").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_result_file(&link, MAX_BODY_BYTES).is_err());
        assert_eq!(
            read_result_file(&target, MAX_BODY_BYTES).unwrap(),
            b"real content"
        );
    }

    #[test]
    fn ac2_over_cap_refused() {
        let dir = TempDir::new("cap");
        let exact = dir.0.join("exact.md");
        let over = dir.0.join("over.md");
        fs::write(&exact, vec![b'a'; MAX_BODY_BYTES]).unwrap();
        fs::write(&over, vec![b'a'; MAX_BODY_BYTES + 1]).unwrap();
        assert_eq!(
            read_result_file(&exact, MAX_BODY_BYTES).unwrap().len(),
            MAX_BODY_BYTES
        );
        let err = read_result_file(&over, MAX_BODY_BYTES).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    // -----------------------------------------------------------------------
    // AC-2 post & outbox tests

    #[test]
    fn ac2_file_with_ids_refused() {
        let dir = TempDir::new("file-ids");
        for key in FORBIDDEN_FILE_KEYS {
            let mut doc = sample_post_json();
            doc[key] = json!("forbidden-value");
            let path = dir.0.join(format!("bad-{key}.json"));
            fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
            let mut api = FakeApi::new();
            let mut out = Vec::new();
            let err = execute_with(
                &mut api,
                Some(7),
                TimelineCommand::Post(PostArgs {
                    board: Some(BOARD.to_string()),
                    run: Some(RUN_1.to_string()),
                    producer: Some(WORKER.to_string()),
                    request_id: None,
                    source: PostSource::File(path),
                }),
                Some(&dir.0.join("outbox")),
                None,
                &mut out,
            )
            .unwrap_err();
            assert_eq!(err.code, EXIT_ERROR);
            assert!(err.message.contains(key), "{}", err.message);
            assert!(api.calls.is_empty());
        }
    }

    #[test]
    fn ac2_default_request_id_is_stable() {
        let a: PostInput = serde_json::from_value(sample_post_json()).unwrap();
        let b: PostInput = serde_json::from_str(
            r###"{
                "title": "入力検証を追加",
                "summary": "境界値のテストを追加。GUI確認は未実施。",
                "bodyMarkdown": "## 実施内容\n入力値の境界チェックを追加した。",
                "outcome": "partial",
                "kind": "summary",
                "schemaVersion": 1,
                "sourceRef": {
                    "eventId": "ev-1",
                    "runId": "r1",
                    "sessionId": "s1",
                    "namespace": "worker",
                    "locator": null
                },
                "checks": [
                    {"reportedBy": "worker", "result": "passed", "name": "単体テスト", "evidenceRef": null},
                    {"reportedBy": "worker", "result": "not_run", "name": "GUI確認"}
                ],
                "openIssues": [{"text": "GUIでの動作確認", "ref": null}],
                "nextActions": [{"text": "対象画面で境界値を確認する", "ref": "docs/check.md"}],
                "artifacts": [],
                "basedOnPostIds": [],
                "author": null
            }"###,
        )
        .unwrap();
        let a = a.normalized();
        let b = b.normalized();
        let id_a = default_request_id(BOARD, Some(RUN_1), WORKER, &a);
        let id_b = default_request_id(BOARD, Some(RUN_1), WORKER, &b);
        assert_eq!(id_a, id_b);
        assert!(id_a.starts_with("cli-"));
        assert!(valid_token(&id_a));

        let id_other_run = default_request_id(BOARD, Some(RUN_2), WORKER, &a);
        assert_ne!(id_a, id_other_run);
        let id_no_run = default_request_id(BOARD, None, WORKER, &a);
        assert_ne!(id_a, id_no_run);
    }

    #[test]
    fn ac2_outbox_written_before_send_and_removed_after_ack() {
        let dir = TempDir::new("outbox-lifecycle");
        let file = dir.0.join("summary.json");
        let outbox_dir = dir.0.join("outbox");
        fs::write(&file, serde_json::to_vec(&sample_post_json()).unwrap()).unwrap();

        let outbox_check = outbox_dir.clone();
        let mut attempt = 0u32;
        let mut api = FakeApi::new();
        api.post_hook = Some(Box::new(move |params: &PostParams| {
            attempt += 1;
            // The envelope must already be durable on disk before sending.
            let outbox = Outbox::open(outbox_check.clone(), false).unwrap();
            let entries = outbox.entries().unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].envelope.request_id, params.request_id);
            if attempt == 1 {
                Err(ClientError::Rpc {
                    code: -32014,
                    message: "busy".to_string(),
                })
            } else {
                Ok(PostResult {
                    created_at: "2026-11-26T00:00:00Z".to_string(),
                    duplicate: false,
                    post_id: POST_1.to_string(),
                    seq: Seq(1),
                })
            }
        }));

        let make_cmd = || {
            TimelineCommand::Post(PostArgs {
                board: Some(BOARD.to_string()),
                run: Some(RUN_1.to_string()),
                producer: Some(WORKER.to_string()),
                request_id: None,
                source: PostSource::File(file.clone()),
            })
        };

        // First attempt fails -> error exit, envelope stays in outbox.
        let mut out1 = Vec::new();
        let err = execute_with(
            &mut api,
            Some(7),
            make_cmd(),
            Some(&outbox_dir),
            None,
            &mut out1,
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        assert!(err.message.contains("busy"), "{}", err.message);
        assert!(out1.is_empty());
        let after_fail = Outbox::open(outbox_dir.clone(), false)
            .unwrap()
            .entries()
            .unwrap();
        assert_eq!(after_fail.len(), 1);
        assert!(matches!(
            after_fail[0].state,
            PublishState::Failed {
                kind: Some(TimelineErrorKind::Busy),
                ..
            }
        ));

        // Second attempt succeeds -> exit 0, same request id, outbox entry removed.
        let mut out2 = Vec::new();
        let code = execute_with(
            &mut api,
            Some(7),
            make_cmd(),
            Some(&outbox_dir),
            None,
            &mut out2,
        )
        .unwrap();
        assert_eq!(code, EXIT_OK);
        assert_eq!(api.posted.len(), 2);
        assert_eq!(api.posted[0].request_id, api.posted[1].request_id);
        let printed: PostResult = serde_json::from_slice(&out2).unwrap();
        assert_eq!(printed.post_id, POST_1);
        assert_eq!(printed.seq, Seq(1));
        assert!(
            Outbox::open(outbox_dir, false)
                .unwrap()
                .entries()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn ac2_no_ack_exits_nonzero_and_keeps_envelope() {
        let dir = TempDir::new("no-ack");
        let file = dir.0.join("summary.json");
        let outbox_dir = dir.0.join("outbox");
        fs::write(&file, serde_json::to_vec(&sample_post_json()).unwrap()).unwrap();

        let mut api = FakeApi::new();
        api.post_hook = Some(Box::new(|_| {
            Err(ClientError::Timeout("noa.timeline.post timed out".into()))
        }));
        let mut out = Vec::new();
        let err = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Post(PostArgs {
                board: Some(BOARD.to_string()),
                run: Some(RUN_1.to_string()),
                producer: Some(WORKER.to_string()),
                request_id: Some("req-timeout".to_string()),
                source: PostSource::File(file),
            }),
            Some(&outbox_dir),
            None,
            &mut out,
        )
        .unwrap_err();
        assert_ne!(err.code, EXIT_OK);
        assert!(out.is_empty());
        let entries = Outbox::open(outbox_dir, false).unwrap().entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].envelope.request_id, "req-timeout");
    }

    #[test]
    fn ac2_outbox_conflict_and_write_failure_do_not_send() {
        let dir = TempDir::new("outbox-conflict");
        let file_a = dir.0.join("a.json");
        let file_b = dir.0.join("b.json");
        let outbox_dir = dir.0.join("outbox");
        let doc_a = sample_post_json();
        let mut doc_b = sample_post_json();
        doc_b["title"] = json!("別のタイトル");
        fs::write(&file_a, serde_json::to_vec(&doc_a).unwrap()).unwrap();
        fs::write(&file_b, serde_json::to_vec(&doc_b).unwrap()).unwrap();

        // Fail first send so envelope A stays staged.
        let mut api = FakeApi::new();
        api.post_hook = Some(Box::new(|_| Err(ClientError::Timeout("timeout".into()))));
        let mut out = Vec::new();
        let _ = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Post(PostArgs {
                board: Some(BOARD.to_string()),
                run: Some(RUN_1.to_string()),
                producer: Some(WORKER.to_string()),
                request_id: Some("same-req".to_string()),
                source: PostSource::File(file_a),
            }),
            Some(&outbox_dir),
            None,
            &mut out,
        )
        .unwrap_err();
        assert_eq!(api.posted.len(), 1);

        // Sending different payload under the same request id is refused locally without sending.
        let err = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Post(PostArgs {
                board: Some(BOARD.to_string()),
                run: Some(RUN_1.to_string()),
                producer: Some(WORKER.to_string()),
                request_id: Some("same-req".to_string()),
                source: PostSource::File(file_b.clone()),
            }),
            Some(&outbox_dir),
            None,
            &mut out,
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        assert!(err.message.contains("conflict"), "{}", err.message);
        assert_eq!(api.posted.len(), 1);

        // Unusable outbox path (regular file instead of directory) fails with exit 1, nothing sent.
        let not_a_dir = dir.0.join("blocked-outbox");
        fs::write(&not_a_dir, b"x").unwrap();
        let err = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Post(PostArgs {
                board: Some(BOARD.to_string()),
                run: Some(RUN_1.to_string()),
                producer: Some(WORKER.to_string()),
                request_id: Some("fresh-req".to_string()),
                source: PostSource::File(file_b),
            }),
            Some(&not_a_dir),
            None,
            &mut out,
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        assert_eq!(api.posted.len(), 1);
    }

    #[test]
    fn ac2_from_result_and_take_over() {
        let dir = TempDir::new("from-result");
        let result_path = dir.0.join("result-worker.md");
        fs::write(&result_path, "# Report\nAll checks passed.\n").unwrap();
        let outbox_dir = dir.0.join("outbox");

        let mut api = FakeApi::new();
        api.policy_resps.push_back(Ok(RunInfo {
            publish_policy: PublishPolicy {
                owner: PublishOwner::Manual,
                producer_id: Some(DEFAULT_MANUAL_PRODUCER.to_string()),
            },
            ..sample_run(RUN_1)
        }));
        api.post_hook = Some(Box::new(|_| {
            Ok(PostResult {
                created_at: "2026-11-26T00:00:00Z".to_string(),
                duplicate: false,
                post_id: POST_1.to_string(),
                seq: Seq(1),
            })
        }));

        let mut out = Vec::new();
        let code = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Post(PostArgs {
                board: Some(BOARD.to_string()),
                run: Some(RUN_1.to_string()),
                producer: None,
                request_id: None,
                source: PostSource::FromResult {
                    path: result_path.clone(),
                    title: "Imported report".to_string(),
                    summary: "Imported from NOA_RESULT.".to_string(),
                    outcome: Outcome::Succeeded,
                    kind: PostKind::Summary,
                    take_over: true,
                },
            }),
            Some(&outbox_dir),
            None,
            &mut out,
        )
        .unwrap();
        assert_eq!(code, EXIT_OK);
        assert_eq!(api.calls, vec!["setPublishPolicy", "post"]);
        assert_eq!(api.policy_calls.len(), 1);
        assert_eq!(
            api.policy_calls[0].publish_policy.owner,
            PublishOwner::Manual
        );
        assert_eq!(
            api.policy_calls[0].publish_policy.producer_id.as_deref(),
            Some(DEFAULT_MANUAL_PRODUCER)
        );
        let sent = &api.posted[0];
        assert_eq!(sent.run_id.as_deref(), Some(RUN_1));
        assert_eq!(sent.producer_id, DEFAULT_MANUAL_PRODUCER);
        assert_eq!(sent.post.source_ref.namespace, "result-file");
        assert_eq!(sent.post.source_ref.session_id, RUN_1);
        assert!(sent.post.source_ref.event_id.starts_with("file-"));
        assert_eq!(sent.post.body_markdown, "# Report\nAll checks passed.\n");
        assert_eq!(sent.post.artifacts.len(), 1);
        assert_eq!(sent.post.artifacts[0].kind, "file");
        assert_eq!(
            sent.post.artifacts[0].r#ref,
            result_path.to_string_lossy().as_ref()
        );

        // Without --run, --from-result never guesses a run from the current pane.
        let mut out_unreg = Vec::new();
        let code = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Post(PostArgs {
                board: None,
                run: None,
                producer: None,
                request_id: None,
                source: PostSource::FromResult {
                    path: result_path,
                    title: "Old report".to_string(),
                    summary: "Manual import without run.".to_string(),
                    outcome: Outcome::Unknown,
                    kind: PostKind::Summary,
                    take_over: false,
                },
            }),
            Some(&outbox_dir),
            None,
            &mut out_unreg,
        )
        .unwrap();
        assert_eq!(code, EXIT_OK);
        let unreg = &api.posted[1];
        assert_eq!(unreg.run_id, None);
        assert_eq!(unreg.producer_id, DEFAULT_MANUAL_PRODUCER);
        assert_eq!(unreg.post.source_ref.namespace, "result-file");
        assert_eq!(unreg.post.source_ref.session_id, "none");
    }

    #[test]
    fn ac2_unsupported_server_exits_3_and_names_capability_and_result() {
        let mut api = FakeApi::new();
        api.supported = false;
        let mut out = Vec::new();
        let err = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Boards {
                before: None,
                limit: None,
            },
            None,
            Some("/repo/.noa/result-w.md"),
            &mut out,
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_UNAVAILABLE);
        assert!(err.message.contains(CAPABILITY_BOARD_TIMELINE_V1));
        assert!(err.message.contains("/repo/.noa/result-w.md"));
        assert!(api.calls.is_empty());
    }

    #[test]
    fn ac2_post_without_run_selects_unique_and_refuses_zero_or_many() {
        let dir = TempDir::new("select-run");
        let file = dir.0.join("summary.json");
        let outbox_dir = dir.0.join("outbox");
        fs::write(&file, serde_json::to_vec(&sample_post_json()).unwrap()).unwrap();

        let make_cmd = || {
            TimelineCommand::Post(PostArgs {
                board: None,
                run: None,
                producer: None,
                request_id: None,
                source: PostSource::File(file.clone()),
            })
        };

        // Zero runs -> refused with guidance.
        let mut api = FakeApi::new();
        api.context_resp = Some(sample_context(vec![]));
        let mut out = Vec::new();
        let err = execute_with(
            &mut api,
            Some(7),
            make_cmd(),
            Some(&outbox_dir),
            None,
            &mut out,
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        assert!(err.message.contains("no registered run"), "{}", err.message);

        // Two runs -> refused listing both run ids.
        let mut api = FakeApi::new();
        api.context_resp = Some(sample_context(vec![sample_run(RUN_1), sample_run(RUN_2)]));
        let err = execute_with(
            &mut api,
            Some(7),
            make_cmd(),
            Some(&outbox_dir),
            None,
            &mut out,
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        assert!(err.message.contains(RUN_1) && err.message.contains(RUN_2));

        // Exactly one run -> automatically chosen with its worker producer id.
        let mut api = FakeApi::new();
        api.context_resp = Some(sample_context(vec![sample_run(RUN_1)]));
        api.post_hook = Some(Box::new(|_| {
            Ok(PostResult {
                created_at: "2026-11-26T00:00:00Z".to_string(),
                duplicate: false,
                post_id: POST_1.to_string(),
                seq: Seq(1),
            })
        }));
        let code = execute_with(
            &mut api,
            Some(7),
            make_cmd(),
            Some(&outbox_dir),
            None,
            &mut out,
        )
        .unwrap();
        assert_eq!(code, EXIT_OK);
        assert_eq!(api.posted[0].run_id.as_deref(), Some(RUN_1));
        assert_eq!(api.posted[0].producer_id, WORKER);
    }

    // -----------------------------------------------------------------------
    // AC-2 export tests

    #[test]
    fn ac2_export_markdown_and_json_keep_ids() {
        let board = sample_context(vec![sample_run(RUN_1)]);
        let posts = vec![
            sample_post(POST_1, 1, "First post"),
            sample_post(POST_2, 2, "Second post"),
        ];

        let md = export_markdown(&board, &posts);
        assert_eq!(
            md,
            concat!(
                "# Feature Board\n\n",
                "- Board ID: `bd-0123456789abcdef`\n",
                "- Head Seq: `2`\n",
                "- Archived: `false`\n",
                "- Posts: `2`\n",
                "\n---\n\n",
                "## #1 First post\n\n",
                "- Board ID: `bd-0123456789abcdef`\n",
                "- Post ID: `po-0000000000000001`\n",
                "- Seq: `1`\n",
                "- Created At: `2026-11-26T00:01:00Z`\n",
                "- Occurred At: `2026-11-26T00:01:00Z`\n",
                "- Kind: `summary`\n",
                "- Outcome: `partial`\n",
                "- Provenance: `registered`\n",
                "- Producer: `wk-0123456789abcdef` (`worker`)\n",
                "- Author: ctl-cli, agent=`claude`, role=`implementer`, worker=`wk-0123456789abcdef`\n",
                "- Worker ID: `wk-0123456789abcdef`\n",
                "- Run ID: `rn-0123456789abcdef`\n",
                "- Task: `github:42` Add timeline (https://example.test/42)\n",
                "- Source Ref: namespace=`worker` sessionId=`s1` runId=`r1` eventId=`ev-1` locator=`line:1`\n",
                "\n### Summary\n\nSummary for po-0000000000000001.\n",
                "\n### Body\n\nBody for po-0000000000000001.\n",
                "\n### Checks\n\n- [passed] unit (reported by `worker`, evidence: `ci/1`)\n",
                "\n### Artifacts\n\n- [file] secret-should-not-inline.txt: `/nonexistent/secret-should-not-inline.txt` (digest: `sha256:abc`)\n",
                "\n### Open Issues\n\n- check gui (`issue-1`)\n",
                "\n### Next Actions\n\n- ship it\n",
                "\n---\n\n",
                "## #2 Second post\n\n",
                "- Board ID: `bd-0123456789abcdef`\n",
                "- Post ID: `po-0000000000000002`\n",
                "- Seq: `2`\n",
                "- Created At: `2026-11-26T00:02:00Z`\n",
                "- Occurred At: `2026-11-26T00:02:00Z`\n",
                "- Kind: `summary`\n",
                "- Outcome: `partial`\n",
                "- Provenance: `registered`\n",
                "- Producer: `wk-0123456789abcdef` (`worker`)\n",
                "- Author: ctl-cli, agent=`claude`, role=`implementer`, worker=`wk-0123456789abcdef`\n",
                "- Worker ID: `wk-0123456789abcdef`\n",
                "- Run ID: `rn-0123456789abcdef`\n",
                "- Task: `github:42` Add timeline (https://example.test/42)\n",
                "- Source Ref: namespace=`worker` sessionId=`s1` runId=`r1` eventId=`ev-2` locator=`line:1`\n",
                "\n### Summary\n\nSummary for po-0000000000000002.\n",
                "\n### Body\n\nBody for po-0000000000000002.\n",
                "\n### Checks\n\n- [passed] unit (reported by `worker`, evidence: `ci/1`)\n",
                "\n### Artifacts\n\n- [file] secret-should-not-inline.txt: `/nonexistent/secret-should-not-inline.txt` (digest: `sha256:abc`)\n",
                "\n### Open Issues\n\n- check gui (`issue-1`)\n",
                "\n### Next Actions\n\n- ship it\n",
            )
        );

        let json_text = export_json(&board, &posts);
        let parsed: serde_json::Value = serde_json::from_str(&json_text).unwrap();
        assert_eq!(parsed["schemaVersion"], 1);
        assert_eq!(parsed["boardId"], BOARD);
        assert_eq!(parsed["headSeq"], "2");
        assert_eq!(parsed["posts"].as_array().unwrap().len(), 2);
        for (idx, (pid, seq, ev)) in [(POST_1, "1", "ev-1"), (POST_2, "2", "ev-2")]
            .into_iter()
            .enumerate()
        {
            let p = &parsed["posts"][idx];
            assert_eq!(p["boardId"], BOARD);
            assert_eq!(p["postId"], pid);
            assert_eq!(p["seq"], seq);
            assert_eq!(p["sourceRef"]["namespace"], "worker");
            assert_eq!(p["sourceRef"]["eventId"], ev);
        }
    }

    #[test]
    fn ac2_export_is_atomic() {
        let dir = TempDir::new("export-atomic");
        let existing_out = dir.0.join("existing.md");
        fs::write(&existing_out, "previous content\n").unwrap();
        let fresh_out = dir.0.join("fresh.json");

        let p1 = sample_post(POST_1, 1, "One");
        let p2 = sample_post(POST_2, 2, "Two");

        // Failure mid-fetch leaves an existing output untouched and writes no staging leftovers.
        let mut api = FakeApi::new();
        api.changes_resps.push_back(Ok(PageResult {
            has_more: true,
            head_seq: Seq(2),
            next_after_seq: Some(Seq(1)),
            next_before_seq: None,
            posts: vec![summary_of(&p1)],
        }));
        api.changes_resps.push_back(Ok(PageResult {
            has_more: false,
            head_seq: Seq(2),
            next_after_seq: Some(Seq(2)),
            next_before_seq: None,
            posts: vec![summary_of(&p2)],
        }));
        api.get_resps.push_back(Ok(GetResult {
            post: p1.clone(),
            superseded_by: Vec::new(),
        }));
        api.get_resps.push_back(Err(ClientError::Rpc {
            code: -32016,
            message: "disk read failed".to_string(),
        }));

        let mut stdout = Vec::new();
        let err = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Export {
                board: Some(BOARD.to_string()),
                format: ExportFormat::Markdown,
                output: existing_out.clone(),
            },
            None,
            None,
            &mut stdout,
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        assert_eq!(
            fs::read_to_string(&existing_out).unwrap(),
            "previous content\n"
        );
        assert!(stdout.is_empty());

        // Fresh path does not exist after a failed fetch either.
        let mut api = FakeApi::new();
        api.changes_resps
            .push_back(Err(ClientError::Timeout("timeout".into())));
        let err = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Export {
                board: Some(BOARD.to_string()),
                format: ExportFormat::Json,
                output: fresh_out.clone(),
            },
            None,
            None,
            &mut stdout,
        )
        .unwrap_err();
        assert_ne!(err.code, EXIT_OK);
        assert!(!fresh_out.exists());

        // Successful multi-page export replaces atomically and emits one JSON document.
        let mut api = FakeApi::new();
        api.changes_resps.push_back(Ok(PageResult {
            has_more: true,
            head_seq: Seq(2),
            next_after_seq: Some(Seq(1)),
            next_before_seq: None,
            posts: vec![summary_of(&p1)],
        }));
        api.changes_resps.push_back(Ok(PageResult {
            has_more: false,
            head_seq: Seq(2),
            next_after_seq: Some(Seq(2)),
            next_before_seq: None,
            posts: vec![summary_of(&p2)],
        }));
        api.get_resps.push_back(Ok(GetResult {
            post: p1,
            superseded_by: Vec::new(),
        }));
        api.get_resps.push_back(Ok(GetResult {
            post: p2,
            superseded_by: Vec::new(),
        }));
        let code = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Export {
                board: Some(BOARD.to_string()),
                format: ExportFormat::Json,
                output: fresh_out.clone(),
            },
            None,
            None,
            &mut stdout,
        )
        .unwrap();
        assert_eq!(code, EXIT_OK);
        let summary: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(summary["boardId"], BOARD);
        assert_eq!(summary["format"], "json");
        assert_eq!(summary["posts"], 2);
        let exported: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&fresh_out).unwrap()).unwrap();
        assert_eq!(exported["posts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn ac2_read_subcommands_emit_single_json_document() {
        let p1 = sample_post(POST_1, 1, "One");
        let mut api = FakeApi::new();
        api.boards_resp = Some(BoardsResult {
            boards: vec![BoardInfo {
                archived: false,
                board_id: BOARD.to_string(),
                bytes: 512,
                created_at: "2026-11-26T00:00:00Z".to_string(),
                head_seq: Seq(1),
                last_activity_at: Some("2026-11-26T00:01:00Z".to_string()),
                name: "Feature Board".to_string(),
                posts: 1,
            }],
            has_more: false,
            next_before: None,
        });
        api.list_resp = Some(PageResult {
            has_more: false,
            head_seq: Seq(1),
            next_after_seq: None,
            next_before_seq: Some(Seq(1)),
            posts: vec![summary_of(&p1)],
        });
        api.get_resps.push_back(Ok(GetResult {
            post: p1.clone(),
            superseded_by: Vec::new(),
        }));
        api.changes_resps.push_back(Ok(PageResult {
            has_more: false,
            head_seq: Seq(1),
            next_after_seq: Some(Seq(1)),
            next_before_seq: None,
            posts: vec![summary_of(&p1)],
        }));

        for cmd in [
            TimelineCommand::Context { pane: None },
            TimelineCommand::Boards {
                before: None,
                limit: Some(10),
            },
            TimelineCommand::List {
                board: None,
                before: None,
                limit: Some(20),
                filter: Filter::default(),
                query: None,
            },
            TimelineCommand::Show {
                board: None,
                post: POST_1.to_string(),
            },
            TimelineCommand::Changes {
                board: None,
                after: 0,
                limit: None,
            },
        ] {
            let mut out = Vec::new();
            let code = execute_with(&mut api, Some(7), cmd, None, None, &mut out).unwrap();
            assert_eq!(code, EXIT_OK);
            let text = String::from_utf8(out).unwrap();
            assert_eq!(text.lines().count(), 1, "must print one JSON line: {text}");
            let _: serde_json::Value = serde_json::from_str(&text).unwrap();
        }
    }

    // -----------------------------------------------------------------------
    // Review fixes: D1 exit codes, P1 take-over order, P2 bounded --file,
    // P5 unknown producer, P11 boards paging

    fn list_cmd() -> TimelineCommand {
        TimelineCommand::List {
            board: Some(BOARD.to_string()),
            before: None,
            limit: None,
            filter: Filter::default(),
            query: None,
        }
    }

    fn outcome_unknown(message: &str) -> ClientError {
        ClientError::Rpc {
            code: TimelineErrorKind::OutcomeUnknown.code().code(),
            message: message.to_string(),
        }
    }

    fn acked(_: &PostParams) -> Result<PostResult, ClientError> {
        Ok(PostResult {
            created_at: "2026-11-26T00:00:00Z".to_string(),
            duplicate: false,
            post_id: POST_1.to_string(),
            seq: Seq(1),
        })
    }

    fn from_result_cmd(
        path: &Path,
        run: &str,
        request_id: Option<&str>,
        take_over: bool,
    ) -> TimelineCommand {
        TimelineCommand::Post(PostArgs {
            board: Some(BOARD.to_string()),
            run: Some(run.to_string()),
            producer: None,
            request_id: request_id.map(str::to_string),
            source: PostSource::FromResult {
                path: path.to_path_buf(),
                title: "Imported report".to_string(),
                summary: "Imported from NOA_RESULT.".to_string(),
                outcome: Outcome::Succeeded,
                kind: PostKind::Summary,
                take_over,
            },
        })
    }

    /// Where the outbox keeps the entry for this key (`Outbox::path`).
    fn outbox_entry_path(dir: &Path, producer: &str, request_id: &str) -> PathBuf {
        let hash = fnv1a64(&[
            BOARD.as_bytes(),
            b"\0",
            producer.as_bytes(),
            b"\0",
            request_id.as_bytes(),
        ]);
        dir.join(format!("{hash:016x}.json"))
    }

    #[test]
    fn ac2_unconfirmed_reads_exit_1_and_only_the_capability_is_exit_3() {
        let run = |err: ClientError| {
            let mut api = FakeApi::new();
            api.read_errors.push_back(err);
            let mut out = Vec::new();
            let failure =
                execute_with(&mut api, Some(7), list_cmd(), None, None, &mut out).unwrap_err();
            assert!(out.is_empty());
            failure
        };

        // The service queued the read but gave up after its 4 s wait.
        let err = run(outcome_unknown("no reply within 4 s"));
        assert_eq!(err.code, EXIT_ERROR);
        assert!(err.message.contains("outcome_unknown"), "{}", err.message);
        assert!(
            err.message.contains("no reply within 4 s"),
            "{}",
            err.message
        );
        assert!(!err.message.contains("`ls`"), "{}", err.message);

        // The client's own deadline.
        let err = run(ClientError::Timeout(
            "no response to noa.timeline.list within the request deadline".into(),
        ));
        assert_eq!(err.code, EXIT_ERROR);
        assert!(
            err.message.contains("did not answer the timeline request"),
            "{}",
            err.message
        );

        // The server's own `unsupported` kind is not the missing capability.
        let err = run(ClientError::Rpc {
            code: TimelineErrorKind::Unsupported.code().code(),
            message: "schemaVersion 2 is not supported".to_string(),
        });
        assert_eq!(err.code, EXIT_ERROR);

        // Ctl-wide unavailability keeps exit 3: a lost connection, a missing scope.
        assert_eq!(
            run(ClientError::Closed("going away".into())).code,
            EXIT_UNAVAILABLE
        );
        assert_eq!(
            run(ClientError::Rpc {
                code: -32003,
                message: "requires read scope".to_string(),
            })
            .code,
            EXIT_UNAVAILABLE
        );

        // `boards` and the pane-context lookup go through the same mapping.
        let mut api = FakeApi::new();
        api.read_errors
            .push_back(ClientError::Timeout("deadline".into()));
        let err = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Boards {
                before: None,
                limit: None,
            },
            None,
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        let mut api = FakeApi::new();
        api.read_errors.push_back(outcome_unknown("late"));
        let err = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Changes {
                board: None,
                after: 0,
                limit: None,
            },
            None,
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
    }

    #[test]
    fn ac2_unconfirmed_writes_exit_1_and_name_the_request_id() {
        let dir = TempDir::new("unconfirmed-writes");
        let file = dir.0.join("summary.json");
        let outbox_dir = dir.0.join("outbox");
        fs::write(&file, serde_json::to_vec(&sample_post_json()).unwrap()).unwrap();
        let post_cmd = |request_id: Option<&str>| {
            TimelineCommand::Post(PostArgs {
                board: Some(BOARD.to_string()),
                run: Some(RUN_1.to_string()),
                producer: Some(WORKER.to_string()),
                request_id: request_id.map(str::to_string),
                source: PostSource::File(file.clone()),
            })
        };

        // outcome_unknown on the post: exit 1 naming the request id, envelope kept.
        let mut api = FakeApi::new();
        api.post_hook = Some(Box::new(|_| Err(outcome_unknown("write queued, no reply"))));
        let err = execute_with(
            &mut api,
            Some(7),
            post_cmd(Some("req-unknown")),
            Some(&outbox_dir),
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        assert!(err.message.contains("outcome_unknown"), "{}", err.message);
        assert!(
            err.message.contains("requestId req-unknown"),
            "{}",
            err.message
        );
        let entries = Outbox::open(outbox_dir.clone(), false)
            .unwrap()
            .entries()
            .unwrap();
        assert_eq!(entries.len(), 1);

        // A client timeout on the post names the default request id it was sent under.
        let mut api = FakeApi::new();
        api.post_hook = Some(Box::new(|_| Err(ClientError::Timeout("deadline".into()))));
        let err = execute_with(
            &mut api,
            Some(7),
            post_cmd(None),
            Some(&outbox_dir),
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        let sent = &api.posted[0].request_id;
        assert!(sent.starts_with("cli-"));
        assert!(
            err.message.contains(&format!("requestId {sent}")),
            "{}",
            err.message
        );

        // outcome_unknown on the take-over names the policy request id; nothing is posted.
        let result_path = dir.0.join("result.md");
        fs::write(&result_path, "# Report\n").unwrap();
        let mut api = FakeApi::new();
        api.policy_resps
            .push_back(Err(outcome_unknown("policy queued")));
        let err = execute_with(
            &mut api,
            Some(7),
            from_result_cmd(&result_path, RUN_1, None, true),
            Some(&outbox_dir),
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        let policy_id = &api.policy_calls[0].request_id;
        assert!(policy_id.starts_with("cli-policy-"));
        assert!(
            err.message.contains(&format!("requestId {policy_id}")),
            "{}",
            err.message
        );
        assert!(api.posted.is_empty());
    }

    #[test]
    fn ac2_take_over_refused_locally_leaves_policy_unchanged() {
        let dir = TempDir::new("take-over-order");
        let result_path = dir.0.join("result.md");
        fs::write(&result_path, "# Report\nAll checks passed.\n").unwrap();
        let outbox_dir = dir.0.join("outbox");

        // Another post already holds this key: a local conflict.
        let other: PostInput = serde_json::from_value(sample_post_json()).unwrap();
        Outbox::open(outbox_dir.clone(), false)
            .unwrap()
            .stage(&Envelope {
                board_id: BOARD.to_string(),
                post: other.normalized(),
                producer_id: DEFAULT_MANUAL_PRODUCER.to_string(),
                request_id: "take-req".to_string(),
                run_id: Some(RUN_1.to_string()),
            })
            .unwrap();
        let mut api = FakeApi::new();
        let err = execute_with(
            &mut api,
            Some(7),
            from_result_cmd(&result_path, RUN_1, Some("take-req"), true),
            Some(&outbox_dir),
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        assert!(err.message.contains("conflict"), "{}", err.message);
        assert!(api.policy_calls.is_empty());
        assert!(api.posted.is_empty());

        // The outbox cannot write this key's entry.
        fs::create_dir(outbox_entry_path(
            &outbox_dir,
            DEFAULT_MANUAL_PRODUCER,
            "take-req-2",
        ))
        .unwrap();
        let err = execute_with(
            &mut api,
            Some(7),
            from_result_cmd(&result_path, RUN_1, Some("take-req-2"), true),
            Some(&outbox_dir),
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        assert!(api.policy_calls.is_empty());
        assert!(api.posted.is_empty());

        // Staged first, then the policy, then the send.
        api.policy_resps.push_back(Ok(sample_run(RUN_1)));
        api.post_hook = Some(Box::new(acked));
        let code = execute_with(
            &mut api,
            Some(7),
            from_result_cmd(&result_path, RUN_1, Some("take-req-3"), true),
            Some(&outbox_dir),
            None,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(code, EXIT_OK);
        assert_eq!(api.calls, vec!["setPublishPolicy", "post"]);
    }

    #[cfg(unix)]
    #[test]
    fn ac2_post_file_is_read_bounded() {
        let dir = TempDir::new("post-file-bounded");
        let outbox_dir = dir.0.join("outbox");
        let file_cmd = |path: PathBuf| {
            TimelineCommand::Post(PostArgs {
                board: Some(BOARD.to_string()),
                run: Some(RUN_1.to_string()),
                producer: Some(WORKER.to_string()),
                request_id: None,
                source: PostSource::File(path),
            })
        };

        // A FIFO with no writer is refused at once instead of blocking.
        let fifo = dir.0.join("pipe.json");
        let c_path = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: mkfifo reads the NUL-terminated path only.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let mut api = FakeApi::new();
        let started = Instant::now();
        let err = execute_with(
            &mut api,
            Some(7),
            file_cmd(fifo),
            Some(&outbox_dir),
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a FIFO must not hang"
        );
        assert_eq!(err.code, EXIT_ERROR);
        assert!(
            err.message.contains("not a regular file"),
            "{}",
            err.message
        );
        assert!(api.calls.is_empty());

        // A file over the post budget is refused, not read whole.
        let big = dir.0.join("big.json");
        fs::write(&big, vec![b' '; MAX_POST_PARAMS_BYTES + 1]).unwrap();
        let err = execute_with(
            &mut api,
            Some(7),
            file_cmd(big),
            Some(&outbox_dir),
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        assert!(err.message.contains("exceeds"), "{}", err.message);
        assert!(api.calls.is_empty());
    }

    #[test]
    fn ac2_from_result_with_unknown_producer_is_refused() {
        let dir = TempDir::new("unknown-producer");
        let result_path = dir.0.join("result.md");
        fs::write(&result_path, "# Report\n").unwrap();
        let outbox_dir = dir.0.join("outbox");

        // RUN_2 is not on the board: refused, never guessed as cli:manual.
        let mut api = FakeApi::new();
        let err = execute_with(
            &mut api,
            Some(7),
            from_result_cmd(&result_path, RUN_2, None, false),
            Some(&outbox_dir),
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err.code, EXIT_ERROR);
        assert!(err.message.contains("--producer"), "{}", err.message);
        assert!(err.message.contains(RUN_2), "{}", err.message);
        assert!(api.posted.is_empty());
        assert!(!outbox_dir.exists());

        // A known run posts under its own producer.
        api.post_hook = Some(Box::new(acked));
        let code = execute_with(
            &mut api,
            Some(7),
            from_result_cmd(&result_path, RUN_1, None, false),
            Some(&outbox_dir),
            None,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(code, EXIT_OK);
        assert_eq!(api.posted[0].producer_id, WORKER);
    }

    #[test]
    fn ac2_boards_pages_through_before() {
        let cursor = BoardCursor {
            activity_at: "2026-11-26T00:01:00.5Z".to_string(),
            board_id: BOARD.to_string(),
        };
        let raw = format!("{},{}", cursor.activity_at, cursor.board_id);
        assert_eq!(
            parse(&strings(&["boards", "--before", &raw, "--limit", "200"])).unwrap(),
            TimelineCommand::Boards {
                before: Some(cursor.clone()),
                limit: Some(200),
            }
        );
        for bad in [
            "2026-11-26T00:01:00Z",
            &format!(",{BOARD}"),
            "2026-11-26T00:01:00Z,not-a-board",
            "2026-11-26T00:01:00Z,",
        ] {
            assert!(
                parse(&strings(&["boards", "--before", bad])).is_err(),
                "{bad:?} must fail"
            );
        }

        // The cursor reaches the server; the printed nextBefore feeds the next page.
        let mut api = FakeApi::new();
        api.boards_resp = Some(BoardsResult {
            boards: Vec::new(),
            has_more: true,
            next_before: Some(BoardCursor {
                activity_at: "2026-11-25T23:00:00Z".to_string(),
                board_id: "bd-00000000000000ff".to_string(),
            }),
        });
        let mut out = Vec::new();
        let code = execute_with(
            &mut api,
            Some(7),
            TimelineCommand::Boards {
                before: Some(cursor.clone()),
                limit: Some(200),
            },
            None,
            None,
            &mut out,
        )
        .unwrap();
        assert_eq!(code, EXIT_OK);
        assert_eq!(
            api.boards_calls,
            vec![BoardsParams {
                before: Some(cursor),
                limit: Some(200),
            }]
        );
        let printed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let next = &printed["nextBefore"];
        let next_raw = format!(
            "{},{}",
            next["activityAt"].as_str().unwrap(),
            next["boardId"].as_str().unwrap()
        );
        assert_eq!(
            parse(&strings(&["boards", "--before", &next_raw])).unwrap(),
            TimelineCommand::Boards {
                before: api.boards_resp.unwrap().next_before,
                limit: None,
            }
        );
    }

    /// Finding A: A, then B, then A again take over the same run. The third
    /// take-over is a new policy change, not a replay of the first one, while
    /// re-running a command exactly keeps its policy requestId.
    #[test]
    fn take_over_again_by_earlier_producer_is_a_new_policy_change() {
        let dir = TempDir::new("take-over-aba");
        let outbox_dir = dir.0.join("outbox");
        let mut api = FakeApi::new();
        api.post_hook = Some(Box::new(acked));
        let take_over = |api: &mut FakeApi, producer: &str, body: &str| {
            let path = dir.0.join(format!("result-{producer}-{}.md", body.len()));
            fs::write(&path, body).unwrap();
            api.policy_resps.push_back(Ok(sample_run(RUN_1)));
            let code = execute_with(
                api,
                Some(7),
                TimelineCommand::Post(PostArgs {
                    board: Some(BOARD.to_string()),
                    run: Some(RUN_1.to_string()),
                    producer: Some(producer.to_string()),
                    request_id: None,
                    source: PostSource::FromResult {
                        path,
                        title: "Milestone".to_string(),
                        summary: "Taken over.".to_string(),
                        outcome: Outcome::Succeeded,
                        kind: PostKind::Milestone,
                        take_over: true,
                    },
                }),
                Some(&outbox_dir),
                None,
                &mut Vec::new(),
            )
            .unwrap();
            assert_eq!(code, EXIT_OK);
            api.policy_calls.last().unwrap().request_id.clone()
        };
        let first_a = take_over(&mut api, "prod-a", "first by A\n");
        let b = take_over(&mut api, "prod-b", "then B\n");
        let again_a = take_over(&mut api, "prod-a", "A once more\n");
        assert_ne!(first_a, b);
        assert_ne!(
            again_a, first_a,
            "a later take-over by A must not replay A's first policy change"
        );
        // The exact same command again keeps its policy requestId.
        let retry_a = take_over(&mut api, "prod-a", "A once more\n");
        assert_eq!(retry_a, again_a);
    }

    /// Finding C: `post --board B --file F` without `--run`, from a pane on
    /// no board, with a namespace that allows an unregistered post, posts
    /// with no run (as outside a pane) instead of failing on the pane's
    /// `not_found` context. Other namespaces still fail.
    #[test]
    fn file_post_with_board_from_a_pane_on_no_board_posts_unregistered() {
        let dir = TempDir::new("file-no-board-pane");
        let outbox_dir = dir.0.join("outbox");
        let not_found = || ClientError::Rpc {
            code: TimelineErrorKind::NotFound.code().code(),
            message: "pane 7 is on no board".to_string(),
        };
        let cmd = |file: PathBuf| {
            TimelineCommand::Post(PostArgs {
                board: Some(BOARD.to_string()),
                run: None,
                producer: None,
                request_id: None,
                source: PostSource::File(file),
            })
        };
        for namespace in ["manual", "result-file"] {
            let file = dir.0.join(format!("{namespace}.json"));
            let mut doc = sample_post_json();
            doc["sourceRef"]["namespace"] = json!(namespace);
            fs::write(&file, serde_json::to_vec(&doc).unwrap()).unwrap();
            let mut api = FakeApi::new();
            api.read_errors.push_back(not_found());
            api.post_hook = Some(Box::new(acked));
            let code = execute_with(
                &mut api,
                Some(7),
                cmd(file),
                Some(&outbox_dir),
                None,
                &mut Vec::new(),
            )
            .unwrap_or_else(|err| panic!("{namespace}: {}", err.message));
            assert_eq!(code, EXIT_OK);
            assert_eq!(api.posted.len(), 1);
            assert_eq!(api.posted[0].run_id, None);
            assert_eq!(api.posted[0].producer_id, DEFAULT_MANUAL_PRODUCER);
        }

        // A namespace that needs a run still fails on the missing context.
        let file = dir.0.join("worker.json");
        fs::write(&file, serde_json::to_vec(&sample_post_json()).unwrap()).unwrap();
        let mut api = FakeApi::new();
        api.read_errors.push_back(not_found());
        let err = execute_with(
            &mut api,
            Some(7),
            cmd(file),
            Some(&outbox_dir),
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_ne!(err.code, EXIT_OK);
        assert!(api.posted.is_empty());
    }
}
