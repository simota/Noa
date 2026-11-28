//! What agents write is external input: a reviewer's JSON and the lead's
//! JSON are checked here before anything is applied. Every problem is
//! reported precisely, one per entry, because the text goes back to the agent
//! so it can fix its output.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;

use super::types::{
    Confidence, Coverage, Decision, FileStatus, FindingStatus, LeadOp, LeadOutput, Location,
    PartId, RawFinding, RawKey, RoundKind, RoundNo, Session, Severity, Stance, StanceKind,
    TargetKind, TargetManifest, Verdict,
};

/// Everything wrong with one piece of agent output, one problem per entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub problems: Vec<String>,
}

impl ValidationError {
    fn one(problem: impl Into<String>) -> Self {
        Self {
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

/// Longest id a reviewer or the lead may coin.
pub const MAX_ID_LEN: usize = 16;
/// Most findings one reviewer reply may hold.
pub const MAX_FINDINGS: usize = 50;
/// Longest claim, in characters.
pub const MAX_CLAIM: usize = 300;

/// Whether `id` may name a finding: 1–16 ASCII letters, digits, `-` or `_`.
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

/// Whether `loc` names a captured file, and for git targets whether its
/// lines overlap a changed range.
pub fn locate(manifest: &TargetManifest, loc: &Location) -> (bool, Option<bool>) {
    match manifest.file(&loc.path) {
        Some(file) => {
            let in_diff = (!file.changed.is_empty() || manifest.merge_base.is_some()).then(|| {
                // A deleted file is in the diff whole (it is cited by path).
                if file.status == FileStatus::Deleted {
                    return true;
                }
                let Some(line) = loc.line else {
                    // Lines only taken out leave no changed range.
                    return !file.changed.is_empty() || file.deletions > 0;
                };
                let end = loc.end_line.unwrap_or(line);
                file.changed.iter().any(|&(a, b)| line <= b && end >= a)
            });
            (true, in_diff)
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

/// Checks a location against the target, pushing problems prefixed `at`.
fn check_location(
    raw: Option<RawLocation>,
    session: &Session,
    at: &str,
    problems: &mut Vec<String>,
) -> Option<Location> {
    let raw = raw?;
    let before = problems.len();
    // Spaces can be part of a captured file name. Trimming could silently
    // attach evidence to a different file and validate against its lines.
    let path = raw.path.trim_start_matches("./").to_owned();
    if path.is_empty() {
        problems.push(format!("{at}.path is empty"));
    } else if path.starts_with('/') || path.split('/').any(|part| part == "..") {
        problems.push(format!(
            "{at}.path {path:?} must be relative to the target root, without \"..\""
        ));
    }
    if session.target.kind == TargetKind::Document && path != "document.md" {
        problems.push(format!(
            "{at}.path {path:?}: a document target has only \"document.md\" (or use a null location)"
        ));
    }
    if let Some(line) = raw.line
        && line == 0
    {
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

fn one_line(text: &str) -> bool {
    !text.contains('\n')
}

// ---------------------------------------------------------------- reviewer

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReply {
    summary: String,
    #[serde(default)]
    findings: Vec<RawReplyFinding>,
    #[serde(default)]
    stances: Vec<RawStance>,
    #[serde(default)]
    coverage: Option<RawCoverage>,
    #[serde(default)]
    reviewer: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReplyFinding {
    id: serde_json::Value,
    #[serde(default)]
    location: Option<RawLocation>,
    severity: String,
    category: String,
    confidence: String,
    claim: String,
    evidence: String,
    #[serde(default)]
    fix: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStance {
    finding: String,
    stance: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    evidence: Option<String>,
    #[serde(default)]
    severity: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCoverage {
    #[serde(default)]
    reviewed: Vec<String>,
    #[serde(default)]
    skipped: Vec<RawSkipped>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSkipped {
    path: String,
    #[serde(default)]
    reason: String,
}

/// A reviewer's reply, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub summary: String,
    pub findings: Vec<RawFinding>,
    /// `(finding id, stance)`.
    pub stances: Vec<(String, Stance)>,
    pub coverage: Option<Coverage>,
}

fn id_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.trim().to_owned()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Checks reviewer `part`'s JSON for `round` against `session`.
pub fn parse_review(
    text: &str,
    session: &Session,
    part: &PartId,
    round: RoundNo,
) -> Result<Review, ValidationError> {
    let raw: RawReply = serde_json::from_str(strip_fence(text))
        .map_err(|e| ValidationError::one(format!("not valid review JSON: {e}")))?;
    let Some(this_round) = session.round(round) else {
        return Err(ValidationError::one(format!("there is no round {round}")));
    };
    let mut problems = Vec::new();

    if let Some(reviewer) = &raw.reviewer
        && reviewer.trim() != part.as_str()
    {
        problems.push(format!(
            "reviewer: {reviewer:?} is not you; leave it out or write {:?}",
            part.as_str()
        ));
    }
    let summary = raw.summary.trim().to_owned();
    if summary.is_empty() {
        problems.push("\"summary\" is empty".to_owned());
    }

    if raw.findings.len() > MAX_FINDINGS {
        problems.push(format!(
            "findings: {} given, at most {MAX_FINDINGS}; merge related ones and keep the most important",
            raw.findings.len()
        ));
    }
    let categories = session.categories();
    let used_before: BTreeSet<&str> = session
        .raw
        .iter()
        .filter(|r| &r.key.part == part)
        .map(|r| r.key.id.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();
    for (i, f) in raw.findings.into_iter().take(MAX_FINDINGS).enumerate() {
        let at = format!("findings[{i}]");
        let before = problems.len();
        let id = id_text(&f.id).unwrap_or_default();
        if !is_valid_id(&id) {
            problems.push(format!(
                "{at}.id {:?} must be 1-{MAX_ID_LEN} ASCII letters, digits, '-' or '_'",
                f.id
            ));
        } else if used_before.contains(id.as_str()) {
            problems.push(format!(
                "{at}.id {id:?} was used in an earlier round; give new findings new ids"
            ));
        } else if !seen.insert(id.clone()) {
            problems.push(format!("{at}.id {id:?} is used twice in this reply"));
        }
        let location = check_location(
            f.location,
            session,
            &format!("{at}.location"),
            &mut problems,
        );
        let severity = Severity::parse(f.severity.trim());
        if severity.is_none() {
            problems.push(format!(
                "{at}.severity {:?} must be critical, high, medium, low or info",
                f.severity
            ));
        }
        let category = f.category.trim().to_owned();
        if !categories.contains(&category) {
            problems.push(format!(
                "{at}.category {category:?} must be one of {}",
                list(categories.iter().map(String::as_str))
            ));
        }
        let confidence = Confidence::parse(f.confidence.trim());
        if confidence.is_none() {
            problems.push(format!(
                "{at}.confidence {:?} must be high, medium or low",
                f.confidence
            ));
        }
        let claim = f.claim.trim().to_owned();
        if claim.is_empty() {
            problems.push(format!("{at}.claim is empty"));
        } else if !one_line(&claim) {
            problems.push(format!("{at}.claim must be one line"));
        } else if claim.chars().count() > MAX_CLAIM {
            problems.push(format!(
                "{at}.claim has {} characters; at most {MAX_CLAIM} (put details in evidence)",
                claim.chars().count()
            ));
        }
        let evidence = f.evidence.trim().to_owned();
        if evidence.is_empty() {
            problems.push(format!("{at}.evidence is empty"));
        }
        if let (Some(severity), Some(confidence), true) =
            (severity, confidence, problems.len() == before)
        {
            let (in_target, in_diff) = match &location {
                Some(loc) => locate(&session.manifest, loc),
                None => (true, None),
            };
            findings.push(RawFinding {
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
                evidence,
                fix: f.fix.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()),
            });
        }
    }

    let mut stances = Vec::new();
    if this_round.kind == RoundKind::Blind {
        if !raw.stances.is_empty() {
            problems.push(
                "stances: not allowed in the blind round (there are no findings to take a stance on yet)"
                    .to_owned(),
            );
        }
    } else {
        let mut given = BTreeSet::new();
        for (i, s) in raw.stances.into_iter().enumerate() {
            let at = format!("stances[{i}]");
            let before = problems.len();
            let id = s.finding.trim().to_owned();
            let finding = session.finding(&id);
            if finding.is_none() {
                problems.push(format!(
                    "{at}.finding {id:?} is not a finding; findings: {}",
                    list(session.findings.iter().map(|f| f.id.as_str()))
                ));
            } else if let Some(into) = finding
                .filter(|f| f.status == FindingStatus::Merged)
                .map(|f| f.merged_into.as_deref().unwrap_or("another finding"))
            {
                problems.push(format!(
                    "{at}: {id} was merged into {into}; give the stance on {into}"
                ));
            } else if !given.insert(id.clone()) {
                problems.push(format!("{at}: a second stance on {id}"));
            }
            let kind = StanceKind::parse(s.stance.trim());
            match (kind, finding) {
                (None, _) => problems.push(format!(
                    "{at}.stance {:?} must be confirm, dispute, refine, unsure or withdraw",
                    s.stance
                )),
                (Some(kind), Some(finding)) => {
                    let origin = finding.origins().contains(part);
                    if origin && !matches!(kind, StanceKind::Refine | StanceKind::Withdraw) {
                        problems.push(format!(
                            "{at}: you raised {id}; give refine or withdraw, or leave it out"
                        ));
                    }
                    if !origin && kind == StanceKind::Withdraw {
                        problems.push(format!(
                            "{at}: only a reviewer who raised {id} can withdraw it; use dispute"
                        ));
                    }
                }
                _ => {}
            }
            let note = s.note.trim().to_owned();
            if matches!(kind, Some(StanceKind::Dispute | StanceKind::Refine)) && note.is_empty() {
                problems.push(format!("{at}.note is required for {}", s.stance.trim()));
            }
            let severity = match s.severity.as_deref().map(str::trim) {
                None => None,
                Some(name) => {
                    let parsed = Severity::parse(name);
                    if parsed.is_none() {
                        problems.push(format!(
                            "{at}.severity {name:?} must be critical, high, medium, low or info"
                        ));
                    }
                    parsed
                }
            };
            if let (Some(kind), true) = (kind, problems.len() == before) {
                stances.push((
                    id,
                    Stance {
                        round,
                        kind,
                        note,
                        evidence: s
                            .evidence
                            .map(|e| e.trim().to_owned())
                            .filter(|e| !e.is_empty()),
                        severity,
                    },
                ));
            }
        }
        let missing: Vec<&str> = session
            .required_stances(part)
            .into_iter()
            .filter(|id| !given.contains(*id))
            .collect();
        if !missing.is_empty() {
            problems.push(format!(
                "stances: missing {} (give confirm, dispute, refine or unsure)",
                missing.join(", ")
            ));
        }
    }

    let coverage = raw.coverage.map(|c| {
        let mut check = |entry: &str, at: &str| {
            if !session.manifest.matches(entry) {
                problems.push(format!(
                    "{at} {entry:?} is not a target file, a directory ending in '/' that holds one, or \"*\""
                ));
            }
            entry.to_owned()
        };
        let reviewed: Vec<String> = c
            .reviewed
            .iter()
            .enumerate()
            .map(|(i, e)| check(e, &format!("coverage.reviewed[{i}]")))
            .collect();
        let skipped: Vec<(String, String)> = c
            .skipped
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    check(&s.path, &format!("coverage.skipped[{i}].path")),
                    s.reason.trim().to_owned(),
                )
            })
            .collect();
        for (i, (_, reason)) in skipped.iter().enumerate() {
            if reason.is_empty() {
                problems.push(format!("coverage.skipped[{i}].reason is empty"));
            }
        }
        Coverage { reviewed, skipped }
    });

    if !problems.is_empty() {
        return Err(ValidationError { problems });
    }
    Ok(Review {
        summary,
        findings,
        stances,
        coverage,
    })
}

// -------------------------------------------------------------------- lead

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLead {
    summary: String,
    #[serde(default)]
    ops: Vec<RawOp>,
    #[serde(default)]
    questions: BTreeMap<String, String>,
    #[serde(default)]
    verdict: Option<RawVerdict>,
    #[serde(default)]
    close: bool,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum RawOp {
    Add {
        id: String,
        from: Vec<String>,
        claim: String,
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
    must_fix: Vec<String>,
}

/// Whether the lead may close the review in `round`.
pub fn may_close(session: &Session, round: RoundNo) -> bool {
    round >= 2 || session.max_rounds <= 1
}

/// The state of every finding as the lead's ops leave it, for the checks
/// that look at the whole result.
struct Shadow {
    status: BTreeMap<String, FindingStatus>,
}

impl Shadow {
    fn live(&self, id: &str) -> bool {
        self.status.get(id).is_some_and(|s| s.is_live())
    }
}

/// The claim rules a lead's add or update shares with a reviewer's.
fn check_lead_claim(claim: &str, at: &str, problems: &mut Vec<String>) {
    if claim.is_empty() || !one_line(claim) {
        problems.push(format!("{at}.claim must be one non-empty line"));
    } else if claim.chars().count() > MAX_CLAIM {
        problems.push(format!(
            "{at}.claim has {} characters; at most {MAX_CLAIM} (put details in the rationale)",
            claim.chars().count()
        ));
    }
}

/// Checks the lead's JSON for `round` against `session`.
pub fn parse_lead(
    text: &str,
    session: &Session,
    round: RoundNo,
) -> Result<LeadOutput, ValidationError> {
    let raw: RawLead = serde_json::from_str(strip_fence(text))
        .map_err(|e| ValidationError::one(format!("not valid lead JSON: {e}")))?;
    if session.round(round).is_none() {
        return Err(ValidationError::one(format!("there is no round {round}")));
    }
    let mut problems = Vec::new();
    let summary = raw.summary.trim().to_owned();
    if summary.is_empty() {
        problems.push("\"summary\" is empty".to_owned());
    }

    let categories = session.categories();
    // Raw findings of this round, each to be placed exactly once.
    let this_round: Vec<&RawKey> = session
        .raw
        .iter()
        .filter(|r| r.round == round && session.placement(&r.key).is_none())
        .map(|r| &r.key)
        .collect();
    let mut placed: BTreeMap<RawKey, String> = BTreeMap::new();
    let mut shadow = Shadow {
        status: session
            .findings
            .iter()
            .map(|f| (f.id.clone(), f.status))
            .collect(),
    };
    let known_findings = |shadow: &Shadow| list(shadow.status.keys().map(String::as_str));

    // A raw key or finding id in `from`, checked; pushes problems.
    let take_from = |item: &str,
                     into: &str,
                     at: &str,
                     placed: &mut BTreeMap<RawKey, String>,
                     shadow: &mut Shadow,
                     problems: &mut Vec<String>|
     -> bool {
        let item = item.trim();
        if let Some(key) = RawKey::parse(item) {
            if session.raw_finding(&key).is_none() {
                problems.push(format!("{at}: unknown raw finding {item:?}"));
                return false;
            }
            if let Some(where_) = session.placement(&key) {
                problems.push(format!(
                    "{at}: raw finding {item} was placed in an earlier round ({where_})"
                ));
                return false;
            }
            if let Some(prev) = placed.insert(key, at.to_owned()) {
                problems.push(format!(
                    "{at}: raw finding {item} is already placed by {prev}"
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
                    "{at}: {item:?} is neither a raw finding (reviewer/id) nor a live finding; findings: {}",
                    known_findings(shadow)
                ));
                return false;
            }
            shadow.status.insert(item.to_owned(), FindingStatus::Merged);
            true
        }
    };

    let mut ops = Vec::new();
    for (i, op) in raw.ops.into_iter().enumerate() {
        let at = format!("ops[{i}]");
        let before = problems.len();
        match op {
            RawOp::Add {
                id,
                from,
                claim,
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
                    problems.push(format!("{at}: finding {id} already exists"));
                }
                if from.is_empty() {
                    problems.push(format!(
                        "{at}: \"from\" is empty; name the raw findings it is made of"
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
                let status_p = FindingStatus::parse_lead(status.trim());
                if status_p.is_none() {
                    problems.push(format!(
                        "{at}.status {status:?} must be open, agreed, disputed or rejected"
                    ));
                }
                let claim = claim.trim().to_owned();
                check_lead_claim(&claim, &at, &mut problems);
                let rationale = rationale.trim().to_owned();
                if rationale.is_empty() {
                    problems.push(format!("{at}.rationale is empty"));
                }
                if let (Some(severity), Some(confidence), Some(status), true) =
                    (severity_p, confidence_p, status_p, problems.len() == before)
                {
                    shadow.status.insert(id.clone(), status);
                    ops.push(LeadOp::Add {
                        id,
                        from: from.iter().map(|s| s.trim().to_owned()).collect(),
                        claim,
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
                        "{at}: {id:?} is not a live finding; findings: {}",
                        known_findings(&shadow)
                    ));
                }
                if from.is_empty() {
                    problems.push(format!("{at}: \"from\" is empty"));
                }
                for item in &from {
                    take_from(item, &id, &at, &mut placed, &mut shadow, &mut problems);
                }
                if problems.len() == before {
                    ops.push(LeadOp::Merge {
                        id,
                        from: from.iter().map(|s| s.trim().to_owned()).collect(),
                    });
                }
            }
            RawOp::Update {
                id,
                claim,
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
                        "{at}: unknown finding {id:?}; findings: {}",
                        known_findings(&shadow)
                    ));
                } else if shadow.status.get(&id) == Some(&FindingStatus::Merged) {
                    problems.push(format!(
                        "{at}: {id} was merged; update the finding it went into"
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
                let status_p = status
                    .as_deref()
                    .map(|s| (s, FindingStatus::parse_lead(s.trim())));
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
                    check_lead_claim(claim, &at, &mut problems);
                }
                let rationale = rationale.map(|r| r.trim().to_owned());
                if rationale.as_deref().is_some_and(str::is_empty) {
                    problems.push(format!("{at}.rationale is empty"));
                }
                let fix = fix.map(|f| f.trim().to_owned());
                let changes = claim.is_some()
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
                // Withdrawn follows from the reviewers who raised it (each
                // withdrew); the lead cannot bring it back.
                if shadow.status.get(&id) == Some(&FindingStatus::Withdrawn)
                    && new_status.is_some_and(|s| s != FindingStatus::Withdrawn)
                {
                    problems.push(format!(
                        "{at}: {id} was withdrawn by every reviewer who raised it; it cannot be reopened"
                    ));
                }
                if new_status == Some(FindingStatus::Rejected)
                    && rationale.as_deref().is_none_or(str::is_empty)
                {
                    problems.push(format!("{at}: rejecting {id} needs a \"rationale\""));
                }
                if problems.len() == before {
                    if let Some(status) = new_status {
                        shadow.status.insert(id.clone(), status);
                    }
                    ops.push(LeadOp::Update {
                        id,
                        claim,
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
                        "{at}.raw {raw:?} must be a raw finding as reviewer/id (e.g. codex/3)"
                    )),
                    Some(key) => {
                        let ok =
                            take_from(raw.trim(), "", &at, &mut placed, &mut shadow, &mut problems);
                        if ok && problems.len() == before {
                            ops.push(LeadOp::Dismiss { raw: key, reason });
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
            "unassigned raw findings: {} (add, merge or dismiss each one)",
            unassigned.join(", ")
        ));
    }

    let mut questions = BTreeMap::new();
    for (part, question) in raw.questions {
        let part = PartId::new(part.trim());
        if !session.is_reviewer(&part) {
            problems.push(format!(
                "questions: unknown reviewer {:?}; reviewers: {}",
                part.as_str(),
                list(session.reviewer_ids().map(PartId::as_str))
            ));
        } else if question.trim().is_empty() {
            problems.push(format!("questions: the question for {part} is empty"));
        } else {
            questions.insert(part, question.trim().to_owned());
        }
    }

    let last = session.is_last(round);
    if raw.close && !may_close(session, round) {
        problems.push(
            "close: not allowed in round 1; the findings must be cross-examined first".to_owned(),
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
                    "verdict.decision {:?} must be approve, approve_with_fixes, request_changes or reject",
                    v.decision
                ));
            }
            let rationale = v.rationale.trim().to_owned();
            if rationale.is_empty() {
                problems.push("verdict.rationale is empty".to_owned());
            }
            let must_fix: Vec<String> = v.must_fix.iter().map(|s| s.trim().to_owned()).collect();
            for id in &must_fix {
                if !shadow.live(id) {
                    problems.push(format!(
                        "verdict.must_fix: {id:?} is not a live finding (not rejected, withdrawn or merged)"
                    ));
                }
            }
            match decision {
                Some(Decision::Approve) if !must_fix.is_empty() => problems
                    .push("verdict: approve takes no must_fix; use approve_with_fixes".to_owned()),
                Some(d @ (Decision::ApproveWithFixes | Decision::RequestChanges))
                    if must_fix.is_empty() =>
                {
                    problems.push(format!(
                        "verdict: {} needs at least one finding in must_fix",
                        d.name()
                    ));
                }
                _ => {}
            }
            let open: Vec<&str> = shadow
                .status
                .iter()
                .filter(|(_, s)| **s == FindingStatus::Open)
                .map(|(id, _)| id.as_str())
                .collect();
            if !open.is_empty() {
                problems.push(format!(
                    "{} still open; set agreed, disputed or rejected before the verdict",
                    open.join(", ")
                ));
            }
            match (decision, problems.len() == before) {
                (Some(decision), true) => Some(Verdict {
                    decision,
                    rationale,
                    must_fix,
                }),
                _ => None,
            }
        }
    };

    if !problems.is_empty() {
        return Err(ValidationError { problems });
    }
    Ok(LeadOutput {
        summary,
        ops,
        questions,
        verdict,
        close: raw.close,
    })
}

#[cfg(test)]
#[path = "validate_tests.rs"]
mod tests;
