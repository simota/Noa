//! Turns what agents write (L1-6) into checked values: the judge's
//! criteria, a verifier's reply (with the facts the runner found at every
//! cited path, so this stays pure), and the judge's rulings. One code
//! fence is tolerated, unknown fields are refused, and every problem is
//! its own line of the correction note.
//!
//! Agent JSON is read into the private `Raw*` input types below (with
//! `deny_unknown_fields`, as review's), then mapped to the frozen types:
//! `result` → [`Outcome`], a mark's `t` → milliseconds through
//! [`crate::media::parse_mark`], `rulings` an array whose `"same"` copies
//! the previous ruling.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;

use crate::model::types::{
    CRITERIA_ROUND, Confidence, CriteriaOutput, Criterion, CriterionResult, CriterionSource,
    CrossCheck, CrossOutcome, Decision, DefectOp, DefectStatus, Evidence, EvidenceBody,
    EvidenceKey, FileFact, FileState, FileStatus, JudgeOutput, Location, MAX_CAPTION, MAX_CMD,
    MAX_CRITERIA, MAX_DEFECTS, MAX_EVIDENCE_ITEMS, MAX_LOG_BYTES, MAX_MARKS,
    MAX_REPLY_EVIDENCE_BYTES, MAX_RERUNS, MAX_STATEMENT, Mark, Media, Outcome, PartId, Priority,
    RawDefect, RawKey, RoundNo, Ruling, RulingKind, Session, Severity, Subject, TargetKind,
    Verdict,
};

/// Why an agent's file was turned down: one problem per line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub problems: Vec<String>,
}

impl ValidationError {
    pub fn one(problem: impl Into<String>) -> Self {
        ValidationError {
            problems: vec![problem.into()],
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, problem) in self.problems.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationError {}

/// A verifier's validated reply. Its evidence is not stored yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub summary: String,
    pub environment: String,
    pub results: Vec<CriterionResult>,
    pub evidence: Vec<Evidence>,
    pub defects: Vec<RawDefect>,
    pub cross_checks: Vec<CrossCheck>,
}

/// Longest id an agent may coin (evidence, defects).
pub const MAX_ID_LEN: usize = 16;
/// Longest defect claim, in characters.
pub const MAX_CLAIM: usize = 300;
/// The directory, under a verifier's `r<N>/<verifier>/`, the widget keeps
/// for its re-run script and logs (L1-5): no reported path may be in it.
pub const RESERVED_DIR: &str = crate::rerun::RERUN_DIR;

/// Whether `id` may name an evidence item or a defect: 1–16 ASCII letters,
/// digits, `-` or `_`.
pub fn is_valid_id(id: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Drops one Markdown code fence around the whole text, which agents often
/// add even when told not to.
fn strip_fence(text: &str) -> &str {
    let trimmed = text.trim().trim_start_matches('\u{feff}');
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let Some((_, inner)) = rest.split_once('\n') else {
        return trimmed;
    };
    inner.trim_end().strip_suffix("```").unwrap_or(inner).trim()
}

fn list<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let items: Vec<&str> = items.into_iter().collect();
    if items.is_empty() {
        "(none)".to_owned()
    } else {
        items.join(", ")
    }
}

fn one_line(text: &str) -> bool {
    !text.contains('\n')
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

fn criteria_ids(session: &Session) -> String {
    list(session.criteria.iter().map(|c| c.id.as_str()))
}

/// A path a reply cites, relative to its `r<N>/<verifier>/`, as it is kept:
/// trimmed, a leading `./` dropped. The error is the rule it breaks.
pub fn reported_path(path: &str) -> Result<String, String> {
    let mut path = path.trim();
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    if path.is_empty() {
        return Err("is empty".to_owned());
    }
    if path.starts_with('/') {
        return Err("must be relative to your reply dir, not absolute".to_owned());
    }
    if path
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err("must be a plain relative path, without \"..\"".to_owned());
    }
    if path == RESERVED_DIR || path.starts_with(&format!("{RESERVED_DIR}/")) {
        return Err(format!(
            "is under {RESERVED_DIR}/, which the widget keeps for its re-runs; save it elsewhere"
        ));
    }
    Ok(path.to_owned())
}

/// A command's `cwd`, relative to the worktree: `.` or a plain relative
/// path without `..`.
fn reported_cwd(cwd: &str) -> Result<String, String> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return Err("is empty (use \".\" for the worktree's top level)".to_owned());
    }
    if cwd.starts_with('/') {
        return Err("must be relative to your worktree, not absolute".to_owned());
    }
    if cwd.split('/').any(|c| c == "..") {
        return Err("must not contain \"..\"".to_owned());
    }
    Ok(cwd.to_owned())
}

/// Whether `loc` names a captured file, and for a diff target whether its
/// lines overlap a changed range. A state target is the whole tree, so
/// every location is in it.
pub fn locate(session: &Session, loc: &Location) -> (bool, Option<bool>) {
    let manifest = &session.manifest;
    if session.target.kind == TargetKind::State {
        return (true, None);
    }
    match manifest.file(&loc.path) {
        Some(file) => {
            if file.status == FileStatus::Deleted {
                return (true, Some(true));
            }
            let Some(line) = loc.line else {
                return (true, Some(true));
            };
            let end = loc.end_line.unwrap_or(line);
            (
                true,
                Some(file.changed.iter().any(|&(a, b)| line <= b && end >= a)),
            )
        }
        None => (manifest.is_omitted(&loc.path), None),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocation {
    path: String,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    end_line: Option<u32>,
}

/// Checks a location, pushing problems prefixed `at`.
fn check_location(
    raw: Option<RawLocation>,
    session: &Session,
    at: &str,
    problems: &mut Vec<String>,
) -> Option<Location> {
    let raw = raw?;
    let before = problems.len();
    let path = raw.path.trim().trim_start_matches("./").to_owned();
    if path.is_empty() {
        problems.push(format!("{at}.path is empty"));
    } else if path.starts_with('/') || path.split('/').any(|part| part == "..") {
        problems.push(format!(
            "{at}.path {path:?} must be relative to the repository root, without \"..\""
        ));
    }
    if raw.line == Some(0) {
        problems.push(format!("{at}.line must be 1 or more"));
    }
    if raw.end_line.is_some() && raw.line.is_none() {
        problems.push(format!("{at}.end_line needs a line"));
    }
    if let (Some(line), Some(end)) = (raw.line, raw.end_line)
        && end < line
    {
        problems.push(format!("{at}.end_line {end} is before line {line}"));
    }
    if let Some(file) = session.manifest.file(&path) {
        let lines = file.lines;
        for (name, value) in [("line", raw.line), ("end_line", raw.end_line)] {
            if let Some(value) = value
                && value > lines
            {
                problems.push(format!(
                    "{at}.{name} {value} is past the end of {path} ({lines} lines in the snapshot)"
                ));
            }
        }
    }
    (problems.len() == before).then_some(Location {
        path,
        line: raw.line,
        end_line: raw.end_line,
    })
}

// -------------------------------------------------------------- criteria

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCriteria {
    summary: String,
    criteria: Vec<RawCriterion>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCriterion {
    id: String,
    statement: String,
    how: String,
    priority: String,
    source: String,
}

/// The judge's `c/criteria.json`, checked against the person's lines and
/// the spec (L1-6, Judge criteria).
pub fn parse_criteria(text: &str, session: &Session) -> Result<CriteriaOutput, ValidationError> {
    let raw: RawCriteria = serde_json::from_str(strip_fence(text))
        .map_err(|e| ValidationError::one(format!("not valid criteria JSON: {e}")))?;
    let mut problems = Vec::new();
    let summary = raw.summary.trim().to_owned();
    if summary.is_empty() {
        problems.push("\"summary\" is empty".to_owned());
    }
    let n = raw.criteria.len();
    if n == 0 {
        problems.push(format!("criteria: none given; write 1 to {MAX_CRITERIA}"));
    } else if n > MAX_CRITERIA {
        problems.push(format!(
            "criteria: {n} given, at most {MAX_CRITERIA}; merge related ones"
        ));
    }
    let mut numbers = BTreeSet::new();
    let mut user_seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut criteria = Vec::new();
    for (i, c) in raw.criteria.iter().enumerate() {
        let at = format!("criteria[{i}]");
        let before = problems.len();
        let id = c.id.trim().to_owned();
        let number = id
            .strip_prefix("AC-")
            .filter(|n| !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<usize>().ok());
        match number {
            None => problems.push(format!("{at}.id {id:?} must be AC-<n> (AC-1, AC-2, …)")),
            Some(number) if !numbers.insert(number) => {
                problems.push(format!("{at}.id {id} is used twice"))
            }
            Some(_) => {}
        }
        let statement = c.statement.trim().to_owned();
        if statement.is_empty() {
            problems.push(format!("{at}.statement is empty"));
        } else if !one_line(&statement) {
            problems.push(format!("{at}.statement must be one line"));
        } else if chars(&statement) > MAX_STATEMENT {
            problems.push(format!(
                "{at}.statement has {} characters; at most {MAX_STATEMENT} (put details in how)",
                chars(&statement)
            ));
        }
        let how = c.how.trim().to_owned();
        if how.is_empty() {
            problems.push(format!("{at}.how is empty; say how to verify it"));
        }
        let priority = Priority::parse(c.priority.trim());
        if priority.is_none() {
            problems.push(format!(
                "{at}.priority {:?} must be must or should",
                c.priority
            ));
        }
        let source = CriterionSource::parse(c.source.trim());
        match source {
            None => problems.push(format!(
                "{at}.source {:?} must be user, request, inferred or spec:<line>",
                c.source
            )),
            Some(CriterionSource::Spec { line }) => match &session.spec {
                None => problems.push(format!(
                    "{at}.source spec:{line}: there is no spec file; use request or inferred"
                )),
                Some(spec) if line > spec.lines => problems.push(format!(
                    "{at}.source spec:{line} is past the end of target/spec.md ({} lines)",
                    spec.lines
                )),
                Some(_) => {}
            },
            Some(CriterionSource::User) => {
                if session.user_criteria.iter().any(|u| u.trim() == statement) {
                    *user_seen.entry(statement.clone()).or_default() += 1;
                } else {
                    problems.push(format!(
                        "{at}: source user, but the statement is not one of the person's lines word for word"
                    ));
                }
            }
            Some(_) => {}
        }
        if let (Some(priority), Some(source), true) = (priority, source, problems.len() == before) {
            criteria.push(Criterion {
                id,
                statement,
                how,
                priority,
                source,
                edited: false,
            });
        }
    }
    if n > 0 && n <= MAX_CRITERIA {
        let missing: Vec<String> = (1..=n)
            .filter(|k| !numbers.contains(k))
            .map(|k| format!("AC-{k}"))
            .collect();
        if !missing.is_empty() && numbers.len() == n {
            problems.push(format!(
                "ids: {} missing; number them AC-1 to AC-{n} without gaps",
                missing.join(", ")
            ));
        }
    }
    // Each line the person gave is one criterion: a line given twice is two
    // criteria with the same statement (AC-5).
    let mut given: Vec<(&str, usize)> = Vec::new();
    for line in session.user_criteria.iter().map(|l| l.trim()) {
        match given.iter_mut().find(|(l, _)| *l == line) {
            Some((_, k)) => *k += 1,
            None => given.push((line, 1)),
        }
    }
    let mut missing_user = Vec::new();
    for (line, want) in given {
        let seen = user_seen.get(line).copied().unwrap_or(0);
        if seen < want {
            missing_user.push(if want == 1 {
                format!("{line:?}")
            } else {
                format!("{line:?} (given {want} times, {seen} here)")
            });
        } else if seen > want {
            let once = if want == 1 {
                "once".to_owned()
            } else {
                format!("{want} times, as the person gave it")
            };
            problems.push(format!(
                "the person's line {line:?} appears {seen} times; give it exactly {once}"
            ));
        }
    }
    if !missing_user.is_empty() {
        problems.push(format!(
            "the person's criteria missing (each once, source user, their wording kept): {}",
            missing_user.join(", ")
        ));
    }
    if !problems.is_empty() {
        return Err(ValidationError { problems });
    }
    Ok(CriteriaOutput { summary, criteria })
}

// ----------------------------------------------------------------- reply

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReply {
    summary: String,
    #[serde(default)]
    environment: String,
    #[serde(default)]
    results: Vec<RawResult>,
    #[serde(default)]
    evidence: Vec<RawEvidence>,
    #[serde(default)]
    defects: Vec<RawReplyDefect>,
    #[serde(default)]
    cross_checks: Vec<RawCross>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResult {
    criterion: String,
    result: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    evidence: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RawEvidence {
    Command {
        id: String,
        cmd: String,
        cwd: String,
        exit: i64,
        log: String,
        #[serde(default)]
        rerun: bool,
    },
    File {
        id: String,
        path: String,
        #[serde(default)]
        caption: Option<String>,
        #[serde(default)]
        marks: Vec<RawMark>,
        #[serde(default)]
        shows: Vec<String>,
    },
    Observation {
        id: String,
        text: String,
    },
}

impl RawEvidence {
    fn id(&self) -> &str {
        match self {
            RawEvidence::Command { id, .. }
            | RawEvidence::File { id, .. }
            | RawEvidence::Observation { id, .. } => id.trim(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMark {
    t: String,
    #[serde(default)]
    note: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReplyDefect {
    id: serde_json::Value,
    #[serde(default)]
    location: Option<RawLocation>,
    severity: String,
    category: String,
    confidence: String,
    claim: String,
    repro: String,
    evidence: Vec<String>,
    #[serde(default)]
    fix: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCross {
    subject: String,
    outcome: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    evidence: Vec<String>,
}

fn id_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.trim().to_owned()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Every `logs/…` and `file` path a reply cites, so the runner can inspect
/// them before [`parse_reply`]; empty when the text is not a reply. Paths
/// that break the path rules ([`reported_path`]) are left out: the reply
/// is turned down for them, and nothing outside the reply dir is looked at.
pub fn cited_paths(text: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(strip_fence(text)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for item in value
        .get("evidence")
        .and_then(|e| e.as_array())
        .into_iter()
        .flatten()
    {
        let field = match item.get("kind").and_then(|k| k.as_str()) {
            Some("command") => "log",
            Some("file") => "path",
            _ => continue,
        };
        if let Some(path) = item.get(field).and_then(|p| p.as_str())
            && let Ok(path) = reported_path(path)
            && !out.contains(&path)
        {
            out.push(path);
        }
    }
    out
}

/// "image" or "video", as problem lines name media.
fn media_word(media: Media) -> &'static str {
    if media.is_video() { "video" } else { "image" }
}

fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

/// The facts found at `path`, checked: it is a regular file of at most
/// `cap` bytes. Returns its size.
fn check_fact(
    fact: Option<&FileFact>,
    path: &str,
    at: &str,
    cap: Option<(u64, &str)>,
    problems: &mut Vec<String>,
) -> Option<u64> {
    let Some(fact) = fact else {
        problems.push(format!("{at}: {path} was not found in your reply dir"));
        return None;
    };
    match fact.state {
        FileState::Missing => {
            problems.push(format!("{at}: {path} does not exist in your reply dir"));
            None
        }
        FileState::Link => {
            problems.push(format!(
                "{at}: {path} is a symbolic link (or behind one); save the file itself"
            ));
            None
        }
        FileState::NotRegular => {
            problems.push(format!("{at}: {path} is not a regular file"));
            None
        }
        FileState::Regular { bytes } => {
            if let Some((cap, what)) = cap
                && bytes > cap
            {
                problems.push(format!(
                    "{at}: {path} is {bytes} bytes; {what} may be at most {} MiB",
                    mib(cap)
                ));
            }
            Some(bytes)
        }
    }
}

/// Verifier `part`'s round-`round` reply, given what was found at every
/// cited path (L1-6, Verifier reply; media per L1-5a).
pub fn parse_reply(
    text: &str,
    session: &Session,
    part: &PartId,
    round: RoundNo,
    files: &[FileFact],
) -> Result<Reply, ValidationError> {
    let raw: RawReply = serde_json::from_str(strip_fence(text))
        .map_err(|e| ValidationError::one(format!("not valid reply JSON: {e}")))?;
    let Some(this_round) = session.round(round) else {
        return Err(ValidationError::one(format!("there is no round {round}")));
    };
    let mut problems = Vec::new();
    let summary = raw.summary.trim().to_owned();
    if summary.is_empty() {
        problems.push("\"summary\" is empty".to_owned());
    }
    let environment = raw.environment.trim().to_owned();
    let fact = |path: &str| files.iter().find(|f| f.path == path);

    // Evidence first: results, defects and cross-checks cite it.
    if raw.evidence.len() > MAX_EVIDENCE_ITEMS {
        problems.push(format!(
            "evidence: {} items, at most {MAX_EVIDENCE_ITEMS}",
            raw.evidence.len()
        ));
    }
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut kinds: BTreeMap<String, bool> = BTreeMap::new(); // id → command or file
    let mut evidence = Vec::new();
    let mut sizes: BTreeMap<String, u64> = BTreeMap::new();
    let mut reruns = 0;
    for (i, item) in raw.evidence.iter().enumerate() {
        let id = item.id().to_owned();
        let at = format!("evidence[{i}] ({id})");
        let before = problems.len();
        if !is_valid_id(&id) {
            problems.push(format!(
                "evidence[{i}].id {id:?} must be 1-{MAX_ID_LEN} ASCII letters, digits, '-' or '_'"
            ));
        } else if !ids.insert(id.clone()) {
            problems.push(format!(
                "evidence[{i}].id {id:?} is used twice in this reply"
            ));
        }
        let body = match item {
            RawEvidence::Command {
                cmd,
                cwd,
                exit,
                log,
                rerun,
                ..
            } => {
                kinds.insert(id.clone(), true);
                let n = chars(cmd);
                if cmd.trim().is_empty() {
                    problems.push(format!("{at}.cmd is empty"));
                } else if n > MAX_CMD {
                    problems.push(format!("{at}.cmd has {n} characters; at most {MAX_CMD}"));
                }
                let cwd = match reported_cwd(cwd) {
                    Ok(cwd) => cwd,
                    Err(why) => {
                        problems.push(format!("{at}.cwd {cwd:?} {why}"));
                        String::new()
                    }
                };
                let exit = match i32::try_from(*exit) {
                    Ok(exit) => exit,
                    Err(_) => {
                        problems.push(format!("{at}.exit {exit} is not an exit status"));
                        0
                    }
                };
                let log = match reported_path(log) {
                    Ok(log) => {
                        if let Some(bytes) = check_fact(
                            fact(&log),
                            &log,
                            &at,
                            Some((MAX_LOG_BYTES, "a log")),
                            &mut problems,
                        ) {
                            sizes.insert(log.clone(), bytes);
                        }
                        log
                    }
                    Err(why) => {
                        problems.push(format!("{at}.log {log:?} {why}"));
                        String::new()
                    }
                };
                if *rerun {
                    reruns += 1;
                }
                EvidenceBody::Command {
                    cmd: cmd.clone(),
                    cwd,
                    exit,
                    log,
                    rerun: *rerun,
                }
            }
            RawEvidence::File {
                path,
                caption,
                marks,
                shows,
                ..
            } => {
                kinds.insert(id.clone(), true);
                let path = match reported_path(path) {
                    Ok(path) => path,
                    Err(why) => {
                        problems.push(format!("{at}.path {path:?} {why}"));
                        String::new()
                    }
                };
                let found = (!path.is_empty()).then(|| fact(&path)).flatten();
                // The media its bytes say, once its extension agrees.
                let media = match found.map(|f| (f.sniffed, f.by_ext)) {
                    Some((Some(a), Some(b))) if a == b => Ok(Some(a)),
                    Some((Some(a), by_ext)) => Err(format!(
                        "{at}: {path} is a {} {} but its extension {}; name it .{}",
                        a.name(),
                        media_word(a),
                        match by_ext {
                            Some(b) => format!("says {}", b.name()),
                            None => "names no image or video".to_owned(),
                        },
                        a.extension()
                    )),
                    Some((None, Some(b))) => Err(format!(
                        "{at}: {path} is named like a {} {} but its content is not one",
                        b.name(),
                        media_word(b)
                    )),
                    _ => Ok(None),
                };
                let media = match media {
                    Ok(media) => Some(media),
                    Err(problem) => {
                        problems.push(problem);
                        None
                    }
                };
                if !path.is_empty() {
                    let cap = match media.flatten() {
                        Some(m) => (
                            crate::media::cap(Some(m)),
                            if m.is_video() { "a video" } else { "an image" },
                        ),
                        None => (crate::media::cap(None), "a file"),
                    };
                    if let Some(bytes) = check_fact(found, &path, &at, Some(cap), &mut problems) {
                        sizes.insert(path.clone(), bytes);
                    }
                }
                let caption = caption
                    .as_deref()
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .map(str::to_owned);
                if let Some(Some(_)) = media
                    && caption.is_none()
                {
                    problems.push(format!(
                        "{at}: {path} is media and needs a caption saying what it shows"
                    ));
                }
                if let Some(caption) = &caption
                    && chars(caption) > MAX_CAPTION
                {
                    problems.push(format!(
                        "{at}.caption has {} characters; at most {MAX_CAPTION}",
                        chars(caption)
                    ));
                }
                let mut parsed = Vec::new();
                if let Some(media) = media
                    && !marks.is_empty()
                {
                    if !media.is_some_and(Media::is_video) {
                        problems.push(format!("{at}.marks: only a video takes marks"));
                    } else if marks.len() > MAX_MARKS {
                        problems.push(format!(
                            "{at}.marks: {} given, at most {MAX_MARKS}",
                            marks.len()
                        ));
                    } else {
                        let mut last = 0;
                        for (j, mark) in marks.iter().enumerate() {
                            let t = mark.t.trim();
                            match crate::media::parse_mark(t) {
                                Err(why) => {
                                    problems.push(format!("{at}.marks[{j}].t {t:?}: {why}"))
                                }
                                Ok(ms) if ms < last => problems.push(format!(
                                    "{at}.marks[{j}].t {t} is before the mark above it; keep marks in time order"
                                )),
                                Ok(ms) => {
                                    last = ms;
                                    parsed.push(Mark {
                                        t_ms: ms,
                                        note: mark.note.trim().to_owned(),
                                    });
                                }
                            }
                            if mark.note.trim().is_empty() {
                                problems.push(format!("{at}.marks[{j}].note is empty"));
                            }
                        }
                    }
                }
                let shows: Vec<String> = shows.iter().map(|s| s.trim().to_owned()).collect();
                for id in &shows {
                    if session.criterion(id).is_none() {
                        problems.push(format!(
                            "{at}.shows: {id:?} is not a criterion; criteria: {}",
                            criteria_ids(session)
                        ));
                    }
                }
                EvidenceBody::File {
                    path,
                    caption,
                    media: media.flatten(),
                    marks: parsed,
                    shows,
                }
            }
            RawEvidence::Observation { text, .. } => {
                kinds.insert(id.clone(), false);
                if text.trim().is_empty() {
                    problems.push(format!("{at}.text is empty"));
                }
                EvidenceBody::Observation {
                    text: text.trim().to_owned(),
                }
            }
        };
        if problems.len() == before {
            evidence.push(Evidence {
                key: EvidenceKey::reported(part, round, &id),
                body,
                stored: None,
                rerun: None,
                attached: None,
            });
        }
    }
    if reruns > MAX_RERUNS {
        problems.push(format!(
            "evidence: {reruns} commands marked rerun: true, at most {MAX_RERUNS}; keep the ones that matter most"
        ));
    }
    let total: u64 = sizes.values().sum();
    if total > MAX_REPLY_EVIDENCE_BYTES {
        problems.push(format!(
            "evidence: the files add up to {total} bytes, at most {} MiB per reply",
            mib(MAX_REPLY_EVIDENCE_BYTES)
        ));
    }
    let mut cited: BTreeSet<String> = BTreeSet::new();
    // Checks `refs` (evidence ids) at `at`: each must be an id of this
    // reply. Returns their keys.
    let mut cite = |refs: &[String], at: &str, problems: &mut Vec<String>| -> Vec<EvidenceKey> {
        let mut keys = Vec::new();
        for r in refs.iter().map(|r| r.trim()) {
            cited.insert(r.to_owned());
            if kinds.contains_key(r) {
                keys.push(EvidenceKey::reported(part, round, r));
            } else {
                problems.push(format!(
                    "{at}.evidence: {r:?} is not an evidence id of this reply; ids: {}",
                    list(kinds.keys().map(String::as_str))
                ));
            }
        }
        keys
    };

    // Results.
    let blind = round == 1;
    let mut given: BTreeSet<String> = BTreeSet::new();
    let mut results = Vec::new();
    for (i, r) in raw.results.iter().enumerate() {
        let criterion = r.criterion.trim().to_owned();
        let at = format!("results[{i}] ({criterion})");
        let before = problems.len();
        if session.criterion(&criterion).is_none() {
            problems.push(format!(
                "results[{i}].criterion {criterion:?} is not a criterion; criteria: {}",
                criteria_ids(session)
            ));
        } else if !given.insert(criterion.clone()) {
            problems.push(format!("results[{i}]: a second result on {criterion}"));
        }
        let outcome = Outcome::parse(r.result.trim());
        let keys = cite(&r.evidence, &at, &mut problems);
        let note = r.note.trim().to_owned();
        match outcome {
            None => problems.push(format!(
                "{at}.result {:?} must be pass, fail, blocked or not_tested",
                r.result
            )),
            Some(Outcome::Pass)
                if !r
                    .evidence
                    .iter()
                    .any(|e| kinds.get(e.trim()).copied() == Some(true)) =>
            {
                problems.push(format!(
                    "{at}: pass needs at least one command or file evidence"
                ));
            }
            Some(Outcome::Fail) => {
                if r.evidence.is_empty() {
                    problems.push(format!("{at}: fail needs evidence"));
                }
                if note.is_empty() {
                    problems.push(format!("{at}: fail needs a note saying what went wrong"));
                }
            }
            Some(o @ (Outcome::Blocked | Outcome::NotTested)) if note.is_empty() => {
                problems.push(format!("{at}: {} needs a note saying why", o.name()));
            }
            _ => {}
        }
        if let (Some(outcome), true) = (outcome, problems.len() == before) {
            results.push(CriterionResult {
                round,
                part: part.clone(),
                criterion,
                outcome,
                note,
                evidence: keys,
            });
        }
    }
    if blind {
        let missing: Vec<&str> = session
            .criteria
            .iter()
            .map(|c| c.id.as_str())
            .filter(|id| !given.contains(*id))
            .collect();
        if !missing.is_empty() {
            problems.push(format!(
                "results: missing {} (one result per criterion in round 1)",
                missing.join(", ")
            ));
        }
    }

    // Defects.
    if raw.defects.len() > MAX_DEFECTS {
        problems.push(format!(
            "defects: {} given, at most {MAX_DEFECTS}; merge related ones",
            raw.defects.len()
        ));
    }
    let categories = session.categories();
    let used_before: BTreeSet<&str> = session
        .raw_defects
        .iter()
        .filter(|r| &r.key.part == part)
        .map(|r| r.key.id.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    let mut defects = Vec::new();
    for (i, d) in raw.defects.into_iter().take(MAX_DEFECTS).enumerate() {
        let at = format!("defects[{i}]");
        let before = problems.len();
        let id = id_text(&d.id).unwrap_or_default();
        if !is_valid_id(&id) {
            problems.push(format!(
                "{at}.id {:?} must be 1-{MAX_ID_LEN} ASCII letters, digits, '-' or '_'",
                d.id
            ));
        } else if used_before.contains(id.as_str()) {
            problems.push(format!(
                "{at}.id {id:?} was used in an earlier round; give new defects new ids"
            ));
        } else if !seen.insert(id.clone()) {
            problems.push(format!("{at}.id {id:?} is used twice in this reply"));
        }
        let location = check_location(
            d.location,
            session,
            &format!("{at}.location"),
            &mut problems,
        );
        let severity = Severity::parse(d.severity.trim());
        if severity.is_none() {
            problems.push(format!(
                "{at}.severity {:?} must be critical, high, medium, low or info",
                d.severity
            ));
        }
        let category = d.category.trim().to_owned();
        if !categories.contains(&category) {
            problems.push(format!(
                "{at}.category {category:?} must be one of {}",
                list(categories.iter().map(String::as_str))
            ));
        }
        let confidence = Confidence::parse(d.confidence.trim());
        if confidence.is_none() {
            problems.push(format!(
                "{at}.confidence {:?} must be high, medium or low",
                d.confidence
            ));
        }
        let claim = d.claim.trim().to_owned();
        check_claim(&claim, &at, &mut problems);
        let repro = d.repro.trim().to_owned();
        if repro.is_empty() {
            problems.push(format!("{at}.repro is empty; give the steps that show it"));
        }
        if d.evidence.is_empty() {
            problems.push(format!(
                "{at}.evidence is empty; cite at least one evidence id"
            ));
        }
        let keys = cite(&d.evidence, &at, &mut problems);
        if let (Some(severity), Some(confidence), true) =
            (severity, confidence, problems.len() == before)
        {
            let (in_target, in_diff) = match &location {
                Some(loc) => locate(session, loc),
                None => (true, None),
            };
            defects.push(RawDefect {
                key: RawKey {
                    part: part.clone(),
                    id,
                },
                round,
                location,
                in_target,
                in_diff,
                severity,
                category,
                confidence,
                claim,
                repro,
                evidence: keys,
                fix: d.fix.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()),
            });
        }
    }

    // Cross-checks.
    let mut cross_checks = Vec::new();
    if blind {
        if !raw.cross_checks.is_empty() {
            problems.push(
                "cross_checks: not allowed in round 1 (there is nothing to cross-check yet)"
                    .to_owned(),
            );
        }
    } else {
        let assigned: Vec<Subject> = this_round
            .assignments
            .get(part)
            .cloned()
            .unwrap_or_default();
        let names = || list_owned(assigned.iter().map(Subject::to_string));
        let mut answered: BTreeSet<Subject> = BTreeSet::new();
        for (i, c) in raw.cross_checks.iter().enumerate() {
            let text = c.subject.trim();
            let at = format!("cross_checks[{i}] ({text})");
            let before = problems.len();
            let subject = Subject::parse(text);
            match &subject {
                None => problems.push(format!(
                    "cross_checks[{i}].subject {text:?} must be <verifier>/<criterion> or a defect id"
                )),
                Some(s) if !assigned.contains(s) => problems.push(format!(
                    "cross_checks[{i}].subject {text} is not one of your assignments: {}",
                    names()
                )),
                Some(s) if !answered.insert(s.clone()) => {
                    problems.push(format!("cross_checks[{i}]: a second cross-check on {text}"))
                }
                Some(_) => {}
            }
            let outcome = CrossOutcome::parse(c.outcome.trim());
            if outcome.is_none() {
                problems.push(format!(
                    "{at}.outcome {:?} must be reproduced, not_reproduced or disputed",
                    c.outcome
                ));
            }
            let note = c.note.trim().to_owned();
            if c.evidence.is_empty() && (outcome != Some(CrossOutcome::Disputed) || note.is_empty())
            {
                problems.push(format!(
                    "{at}: give evidence (only disputed with a note may go without)"
                ));
            }
            let keys = cite(&c.evidence, &at, &mut problems);
            if let (Some(subject), Some(outcome), true) =
                (subject, outcome, problems.len() == before)
            {
                cross_checks.push(CrossCheck {
                    round,
                    by: part.clone(),
                    subject,
                    outcome,
                    note,
                    evidence: keys,
                });
            }
        }
        let missing: Vec<String> = assigned
            .iter()
            .filter(|s| !answered.contains(*s))
            .map(Subject::to_string)
            .collect();
        if !missing.is_empty() {
            problems.push(format!(
                "cross_checks: missing {} (one per assignment)",
                missing.join(", ")
            ));
        }
    }

    for id in kinds.keys() {
        if !cited.contains(id) {
            problems.push(format!(
                "evidence {id:?} is cited by no result, defect or cross-check; cite it or leave it out"
            ));
        }
    }

    if !problems.is_empty() {
        return Err(ValidationError { problems });
    }
    Ok(Reply {
        summary,
        environment,
        results,
        evidence,
        defects,
        cross_checks,
    })
}

fn list_owned(items: impl IntoIterator<Item = String>) -> String {
    let items: Vec<String> = items.into_iter().collect();
    if items.is_empty() {
        "(none)".to_owned()
    } else {
        items.join(", ")
    }
}

fn check_claim(claim: &str, at: &str, problems: &mut Vec<String>) {
    if claim.is_empty() || !one_line(claim) {
        problems.push(format!("{at}.claim must be one non-empty line"));
    } else if chars(claim) > MAX_CLAIM {
        problems.push(format!(
            "{at}.claim has {} characters; at most {MAX_CLAIM} (put details in repro)",
            chars(claim)
        ));
    }
}

// ----------------------------------------------------------------- judge

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJudge {
    summary: String,
    #[serde(default)]
    rulings: Vec<RawRuling>,
    #[serde(default)]
    defect_ops: Vec<RawOp>,
    #[serde(default)]
    assign: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    verdict: Option<RawVerdict>,
    #[serde(default)]
    close: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRuling {
    criterion: String,
    ruling: String,
    #[serde(default)]
    rationale: String,
    #[serde(default)]
    evidence: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum RawOp {
    Add {
        id: String,
        from: Vec<String>,
        claim: String,
        repro: String,
        #[serde(default)]
        location: Option<RawLocation>,
        severity: String,
        category: String,
        confidence: String,
        status: String,
        rationale: String,
        #[serde(default)]
        fix: Option<String>,
    },
    Merge {
        id: String,
        from: Vec<String>,
    },
    Update {
        id: String,
        #[serde(default)]
        claim: Option<String>,
        #[serde(default)]
        repro: Option<String>,
        #[serde(default)]
        location: Option<RawLocation>,
        #[serde(default)]
        severity: Option<String>,
        #[serde(default)]
        category: Option<String>,
        #[serde(default)]
        confidence: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
        #[serde(default)]
        fix: Option<String>,
    },
    Dismiss {
        raw: String,
        reason: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVerdict {
    decision: String,
    rationale: String,
    #[serde(default)]
    blocking: Vec<String>,
}

/// The ruling written to carry the previous one over unchanged.
pub const SAME: &str = "same";

/// Whether the judge may close the verification in `round` with
/// `rulings`, that round's draft rulings on every criterion (`"same"`
/// resolved), before they are accepted (L1-4 item 5; round 1:
/// [`crate::model::rules::early_close_allowed`]).
pub fn may_close(session: &Session, round: RoundNo, rulings: &BTreeMap<String, Ruling>) -> bool {
    round >= 2
        || session.max_rounds <= 1
        || (round == 1 && crate::model::rules::early_close_allowed(session, rulings))
}

/// The state of every defect as the judge's ops leave them.
struct Shadow {
    status: BTreeMap<String, DefectStatus>,
}

impl Shadow {
    fn live(&self, id: &str) -> bool {
        self.status.get(id).is_some_and(|s| s.is_live())
    }
}

/// The judge's round-`round` ruling, held to the evidence standard
/// (`model::rules`) and the verdict check.
pub fn parse_judge(
    text: &str,
    session: &Session,
    round: RoundNo,
) -> Result<JudgeOutput, ValidationError> {
    let raw: RawJudge = serde_json::from_str(strip_fence(text))
        .map_err(|e| ValidationError::one(format!("not valid ruling JSON: {e}")))?;
    if round == CRITERIA_ROUND || session.round(round).is_none() {
        return Err(ValidationError::one(format!("there is no round {round}")));
    }
    let mut problems = Vec::new();
    let summary = raw.summary.trim().to_owned();
    if summary.is_empty() {
        problems.push("\"summary\" is empty".to_owned());
    }

    // Rulings: one per criterion, "same" carried over, each held to the
    // evidence standard.
    let mut rulings: BTreeMap<String, Ruling> = BTreeMap::new();
    let rulings_before = problems.len();
    for (i, r) in raw.rulings.iter().enumerate() {
        let criterion = r.criterion.trim().to_owned();
        let at = format!("rulings[{i}] ({criterion})");
        if session.criterion(&criterion).is_none() {
            problems.push(format!(
                "rulings[{i}].criterion {criterion:?} is not a criterion; criteria: {}",
                criteria_ids(session)
            ));
            continue;
        }
        if rulings.contains_key(&criterion) {
            problems.push(format!("rulings[{i}]: a second ruling on {criterion}"));
            continue;
        }
        let name = r.ruling.trim();
        let ruling = if name == SAME {
            match session.ruling(&criterion) {
                Some(prev) => Ruling {
                    round,
                    ..prev.clone()
                },
                None => {
                    problems.push(format!(
                        "{at}: \"same\" needs an earlier ruling, and {criterion} has none"
                    ));
                    continue;
                }
            }
        } else {
            let Some(kind) = RulingKind::parse(name) else {
                problems.push(format!(
                    "{at}.ruling {name:?} must be verified, failed, inconclusive, not_applicable or same"
                ));
                continue;
            };
            Ruling {
                round,
                kind,
                rationale: r.rationale.trim().to_owned(),
                evidence: r
                    .evidence
                    .iter()
                    .map(|k| EvidenceKey::new(k.trim()))
                    .collect(),
            }
        };
        if let Err(problem) = crate::model::rules::check_ruling(session, round, &criterion, &ruling)
        {
            problems.push(problem);
            continue;
        }
        rulings.insert(criterion, ruling);
    }
    let unruled: Vec<&str> = session
        .criteria
        .iter()
        .map(|c| c.id.as_str())
        .filter(|id| !raw.rulings.iter().any(|r| r.criterion.trim() == *id))
        .collect();
    if !unruled.is_empty() {
        problems.push(format!(
            "rulings: missing {} (one per criterion each round; \"same\" carries one over)",
            unruled.join(", ")
        ));
    }
    let rulings_ok = problems.len() == rulings_before;

    // Defect ops (review's lead ops, with repro).
    let categories = session.categories();
    let this_round: Vec<&RawKey> = session
        .raw_defects
        .iter()
        .filter(|r| r.round == round && session.placement(&r.key).is_none())
        .map(|r| &r.key)
        .collect();
    let mut placed: BTreeMap<RawKey, String> = BTreeMap::new();
    let mut shadow = Shadow {
        status: session
            .defects
            .iter()
            .map(|d| (d.id.clone(), d.status))
            .collect(),
    };
    let ops_before = problems.len();
    let take_from = |item: &str,
                     into: &str,
                     at: &str,
                     placed: &mut BTreeMap<RawKey, String>,
                     shadow: &mut Shadow,
                     problems: &mut Vec<String>|
     -> bool {
        let item = item.trim();
        if let Some(key) = RawKey::parse(item) {
            if session.raw_defect(&key).is_none() {
                problems.push(format!("{at}: unknown raw defect {item:?}"));
                return false;
            }
            if let Some(where_) = session.placement(&key) {
                problems.push(format!(
                    "{at}: raw defect {item} was placed in an earlier round ({where_})"
                ));
                return false;
            }
            if let Some(prev) = placed.insert(key, at.to_owned()) {
                problems.push(format!(
                    "{at}: raw defect {item} is already placed by {prev}"
                ));
                return false;
            }
            true
        } else {
            if item == into {
                problems.push(format!("{at}: {item} cannot be merged into itself"));
                return false;
            }
            if !shadow.live(item) {
                problems.push(format!(
                    "{at}: {item:?} is neither a raw defect (verifier/id) nor a live defect; defects: {}",
                    list(shadow.status.keys().map(String::as_str))
                ));
                return false;
            }
            shadow.status.insert(item.to_owned(), DefectStatus::Merged);
            true
        }
    };
    let parse_status = |name: &str| DefectStatus::parse_judge(name.trim());
    let mut ops = Vec::new();
    for (i, op) in raw.defect_ops.into_iter().enumerate() {
        let at = format!("defect_ops[{i}]");
        let before = problems.len();
        match op {
            RawOp::Add {
                id,
                from,
                claim,
                repro,
                location,
                severity,
                category,
                confidence,
                status,
                rationale,
                fix,
            } => {
                let at = format!("{at} (add)");
                let id = id.trim().to_owned();
                if !is_valid_id(&id) {
                    problems.push(format!(
                        "{at}: id {id:?} must be 1-{MAX_ID_LEN} ASCII letters, digits, '-' or '_'"
                    ));
                } else if shadow.status.contains_key(&id) {
                    problems.push(format!("{at}: defect {id} already exists"));
                }
                if from.is_empty() {
                    problems.push(format!(
                        "{at}: \"from\" is empty; name the raw defects it is made of"
                    ));
                }
                for item in &from {
                    take_from(item, &id, &at, &mut placed, &mut shadow, &mut problems);
                }
                let location =
                    check_location(location, session, &format!("{at}.location"), &mut problems);
                let severity_p = Severity::parse(severity.trim());
                if severity_p.is_none() {
                    problems.push(format!(
                        "{at}.severity {severity:?} must be critical, high, medium, low or info"
                    ));
                }
                let category = category.trim().to_owned();
                if !categories.contains(&category) {
                    problems.push(format!(
                        "{at}.category {category:?} must be one of {}",
                        list(categories.iter().map(String::as_str))
                    ));
                }
                let confidence_p = Confidence::parse(confidence.trim());
                if confidence_p.is_none() {
                    problems.push(format!(
                        "{at}.confidence {confidence:?} must be high, medium or low"
                    ));
                }
                let status_p = parse_status(&status);
                if status_p.is_none() {
                    problems.push(format!(
                        "{at}.status {status:?} must be open, agreed, disputed or rejected"
                    ));
                }
                let claim = claim.trim().to_owned();
                check_claim(&claim, &at, &mut problems);
                let repro = repro.trim().to_owned();
                if repro.is_empty() {
                    problems.push(format!("{at}.repro is empty"));
                }
                let rationale = rationale.trim().to_owned();
                if rationale.is_empty() {
                    problems.push(format!("{at}.rationale is empty"));
                }
                if let (Some(severity), Some(confidence), Some(status), true) =
                    (severity_p, confidence_p, status_p, problems.len() == before)
                {
                    shadow.status.insert(id.clone(), status);
                    ops.push(DefectOp::Add {
                        id,
                        from: from.iter().map(|s| s.trim().to_owned()).collect(),
                        claim,
                        repro,
                        location,
                        severity,
                        category,
                        confidence,
                        status,
                        rationale,
                        fix: fix.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()),
                    });
                }
            }
            RawOp::Merge { id, from } => {
                let at = format!("{at} (merge)");
                let id = id.trim().to_owned();
                if !shadow.live(&id) {
                    problems.push(format!(
                        "{at}: {id:?} is not a live defect; defects: {}",
                        list(shadow.status.keys().map(String::as_str))
                    ));
                }
                if from.is_empty() {
                    problems.push(format!("{at}: \"from\" is empty"));
                }
                for item in &from {
                    take_from(item, &id, &at, &mut placed, &mut shadow, &mut problems);
                }
                if problems.len() == before {
                    ops.push(DefectOp::Merge {
                        id,
                        from: from.iter().map(|s| s.trim().to_owned()).collect(),
                    });
                }
            }
            RawOp::Update {
                id,
                claim,
                repro,
                location,
                severity,
                category,
                confidence,
                status,
                rationale,
                fix,
            } => {
                let at = format!("{at} (update)");
                let id = id.trim().to_owned();
                if !shadow.status.contains_key(&id) {
                    problems.push(format!(
                        "{at}: unknown defect {id:?}; defects: {}",
                        list(shadow.status.keys().map(String::as_str))
                    ));
                } else if shadow.status.get(&id) == Some(&DefectStatus::Merged) {
                    problems.push(format!(
                        "{at}: {id} was merged; update the defect it went into"
                    ));
                }
                let has_location = location.is_some();
                let location =
                    check_location(location, session, &format!("{at}.location"), &mut problems);
                let severity_p = severity.as_deref().map(|s| (s, Severity::parse(s.trim())));
                if let Some((name, None)) = severity_p {
                    problems.push(format!(
                        "{at}.severity {name:?} must be critical, high, medium, low or info"
                    ));
                }
                let confidence_p = confidence
                    .as_deref()
                    .map(|s| (s, Confidence::parse(s.trim())));
                if let Some((name, None)) = confidence_p {
                    problems.push(format!(
                        "{at}.confidence {name:?} must be high, medium or low"
                    ));
                }
                let status_p = status.as_deref().map(|s| (s, parse_status(s)));
                if let Some((name, None)) = status_p {
                    problems.push(format!(
                        "{at}.status {name:?} must be open, agreed, disputed or rejected"
                    ));
                }
                let category = category.map(|c| c.trim().to_owned());
                if let Some(category) = &category
                    && !categories.contains(category)
                {
                    problems.push(format!(
                        "{at}.category {category:?} must be one of {}",
                        list(categories.iter().map(String::as_str))
                    ));
                }
                let claim = claim.map(|c| c.trim().to_owned());
                if let Some(claim) = &claim {
                    check_claim(claim, &at, &mut problems);
                }
                let repro = repro.map(|r| r.trim().to_owned());
                if repro.as_deref() == Some("") {
                    problems.push(format!("{at}.repro is empty"));
                }
                let rationale = rationale.map(|r| r.trim().to_owned());
                let fix = fix.map(|f| f.trim().to_owned());
                let changes = claim.is_some()
                    || repro.is_some()
                    || has_location
                    || severity_p.is_some()
                    || category.is_some()
                    || confidence_p.is_some()
                    || status_p.is_some()
                    || rationale.is_some()
                    || fix.is_some();
                if !changes {
                    problems.push(format!("{at}: changes nothing; give at least one field"));
                }
                let new_status = status_p.and_then(|(_, s)| s);
                if new_status == Some(DefectStatus::Rejected)
                    && rationale.as_deref().is_none_or(str::is_empty)
                {
                    problems.push(format!("{at}: rejecting {id} needs a \"rationale\""));
                }
                if problems.len() == before {
                    if let Some(status) = new_status {
                        shadow.status.insert(id.clone(), status);
                    }
                    ops.push(DefectOp::Update {
                        id,
                        claim,
                        repro,
                        location,
                        severity: severity_p.and_then(|(_, s)| s),
                        category,
                        confidence: confidence_p.and_then(|(_, c)| c),
                        status: new_status,
                        rationale,
                        fix: fix.filter(|f| !f.is_empty()),
                    });
                }
            }
            RawOp::Dismiss { raw, reason } => {
                let at = format!("{at} (dismiss)");
                let reason = reason.trim().to_owned();
                if reason.is_empty() {
                    problems.push(format!("{at}.reason is empty"));
                }
                match RawKey::parse(raw.trim()) {
                    None => problems.push(format!(
                        "{at}.raw {raw:?} must be a raw defect as verifier/id (e.g. codex/3)"
                    )),
                    Some(key) => {
                        let ok =
                            take_from(raw.trim(), "", &at, &mut placed, &mut shadow, &mut problems);
                        if ok && problems.len() == before {
                            ops.push(DefectOp::Dismiss { raw: key, reason });
                        }
                    }
                }
            }
        }
    }
    let unassigned: Vec<String> = this_round
        .iter()
        .filter(|key| !placed.contains_key(**key))
        .map(|key| key.to_string())
        .collect();
    if !unassigned.is_empty() {
        problems.push(format!(
            "unplaced raw defects: {} (add, merge or dismiss each one)",
            unassigned.join(", ")
        ));
    }
    let ops_ok = problems.len() == ops_before;

    // Assignments for the next round.
    let mut assign: BTreeMap<PartId, Vec<Subject>> = BTreeMap::new();
    for (who, subjects) in &raw.assign {
        let who = PartId::new(who.trim());
        if !session.is_verifier(&who) {
            problems.push(format!(
                "assign: {:?} is not a verifier; verifiers: {}",
                who.as_str(),
                list(session.verifier_ids().map(PartId::as_str))
            ));
            continue;
        }
        let mut mine: Vec<Subject> = Vec::new();
        for (j, text) in subjects.iter().enumerate() {
            let text = text.trim();
            let at = format!("assign.{who}[{j}]");
            match Subject::parse(text) {
                None => problems.push(format!(
                    "{at} {text:?} must be <verifier>/<criterion> or a defect id"
                )),
                Some(Subject::Result { part, criterion }) => {
                    if !session.is_verifier(&part) {
                        problems.push(format!("{at}: {part} is not a verifier"));
                    } else if part == who {
                        problems.push(format!(
                            "{at}: {text} is {who}'s own result; a verifier is never assigned its own result"
                        ));
                    } else if session.criterion(&criterion).is_none() {
                        problems.push(format!("{at}: {criterion} is not a criterion"));
                    } else if session.result_as_of(&part, &criterion, round).is_none() {
                        problems.push(format!(
                            "{at}: {part} has no result on {criterion} to reproduce or refute"
                        ));
                    } else {
                        let subject = Subject::Result { part, criterion };
                        if !mine.contains(&subject) {
                            mine.push(subject);
                        }
                    }
                }
                Some(Subject::Defect { id }) => {
                    if !shadow.live(&id) {
                        problems.push(format!("{at}: {id} is not a live defect"));
                    } else {
                        let subject = Subject::Defect { id };
                        if !mine.contains(&subject) {
                            mine.push(subject);
                        }
                    }
                }
            }
        }
        if !mine.is_empty() {
            assign.insert(who, mine);
        }
    }

    // Close and the verdict.
    let last = session.is_last(round);
    if raw.close && rulings_ok && !may_close(session, round, &rulings) {
        problems.push(
            "close: not allowed in round 1 unless every criterion is verified by two verifiers with matched re-runs"
                .to_owned(),
        );
    }
    let verdict = match raw.verdict {
        None => {
            if last || raw.close {
                let why = if last {
                    "this is the last round"
                } else {
                    "\"close\" is true"
                };
                problems.push(format!("verdict: required, {why}"));
            }
            None
        }
        Some(v) => {
            let before = problems.len();
            if !last && !raw.close {
                problems.push("verdict: only in the last round or with \"close\": true".to_owned());
            }
            let decision = Decision::parse(v.decision.trim());
            if decision.is_none() {
                problems.push(format!(
                    "verdict.decision {:?} must be pass, pass_with_issues, fail or inconclusive",
                    v.decision
                ));
            }
            let rationale = v.rationale.trim().to_owned();
            if rationale.is_empty() {
                problems.push("verdict.rationale is empty".to_owned());
            }
            let blocking: Vec<String> = v.blocking.iter().map(|s| s.trim().to_owned()).collect();
            match (decision, problems.len() == before) {
                (Some(decision), true) => {
                    let verdict = Verdict {
                        decision,
                        rationale,
                        blocking,
                    };
                    if rulings_ok && ops_ok {
                        let defects = shadow_defects(session, round, &ops);
                        if let Err(problem) = crate::model::rules::check_verdict(
                            session, &rulings, &defects, &verdict,
                        ) {
                            problems.push(problem);
                        }
                    }
                    Some(verdict)
                }
                _ => None,
            }
        }
    };

    if !problems.is_empty() {
        return Err(ValidationError { problems });
    }
    Ok(JudgeOutput {
        summary,
        rulings,
        defect_ops: ops,
        assign,
        verdict,
        close: raw.close,
    })
}

/// The defects as `ops` (validated) leave them after `round`.
fn shadow_defects(
    session: &Session,
    round: RoundNo,
    ops: &[DefectOp],
) -> Vec<crate::model::types::Defect> {
    let mut shadow = session.clone();
    let output = JudgeOutput {
        summary: String::new(),
        rulings: BTreeMap::new(),
        defect_ops: ops.to_vec(),
        assign: BTreeMap::new(),
        verdict: None,
        close: false,
    };
    match crate::model::reduce::apply(
        &mut shadow,
        &[crate::model::types::Event::JudgeAccepted { round, output }],
    ) {
        Ok(()) => shadow.defects,
        Err(_) => session.defects.clone(),
    }
}

#[cfg(test)]
#[path = "validate_tests.rs"]
mod tests;
