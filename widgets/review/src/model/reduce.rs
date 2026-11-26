//! The event log and its fold: a session is `create` of the first batch,
//! then `apply` of every later one. Each batch bumps `rev` once.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::auto_assign::AutoRecord;

use super::types::{
    Brief, Coverage, Finding, FindingStatus, LeadOp, LeadOutput, Message, Millis, PartId,
    Participant, Phase, RawFinding, RawKey, Receipt, ReviewNote, Round, RoundKind, RoundNo,
    Session, SessionId, Stance, StanceKind, Target, TargetManifest, is_object_id,
};

/// Optional creation metadata, flattened so existing logs keep their original shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<AutoRecord>,
}

impl CreationContext {
    pub(crate) fn new(workdir: Option<PathBuf>, auto: Option<AutoRecord>) -> Option<Self> {
        (workdir.is_some() || auto.is_some()).then_some(Self { workdir, auto })
    }

    fn read<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<Self>, D::Error> {
        let context = Self::deserialize(de)?;
        Ok(Self::new(context.workdir, context.auto))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Always the first event of the log, and only there.
    Created {
        id: SessionId,
        request: String,
        target: Box<Target>,
        manifest: Box<TargetManifest>,
        #[serde(flatten, deserialize_with = "CreationContext::read")]
        workdir: Option<CreationContext>,
        max_rounds: RoundNo,
        lead: Participant,
        reviewers: Vec<Participant>,
        at: Millis,
    },
    PaneAssigned {
        part: PartId,
        pane_id: u64,
    },
    RoundStarted {
        no: RoundNo,
        kind: RoundKind,
        at: Millis,
    },
    MessagePosted {
        message: Message,
    },
    BriefIssued {
        round: RoundNo,
        brief: Brief,
    },
    BriefSent {
        round: RoundNo,
        to: PartId,
        at: Millis,
    },
    BriefStarted {
        round: RoundNo,
        to: PartId,
        at: Millis,
    },
    /// Delivery is tried again: the brief counts as unsent.
    BriefRetried {
        round: RoundNo,
        to: PartId,
        reason: String,
    },
    BriefFailed {
        round: RoundNo,
        to: PartId,
        reason: String,
    },
    /// The user asked to try a failed brief again from the start.
    BriefReset {
        round: RoundNo,
        to: PartId,
    },
    /// A reply file was turned down; the agent is asked to fix it.
    ReplyRejected {
        round: RoundNo,
        to: PartId,
        error: String,
        /// [`crate::store::digest`] of the turned-down reply.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<u64>,
    },
    ReplyAccepted {
        round: RoundNo,
        to: PartId,
        at: Millis,
    },
    /// A reviewer's validated reply: its findings, its stances and the
    /// files it covered.
    ReviewRecorded {
        round: RoundNo,
        part: PartId,
        summary: String,
        #[serde(default)]
        findings: Vec<RawFinding>,
        #[serde(default)]
        stances: Vec<(String, Stance)>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        coverage: Option<Coverage>,
        at: Millis,
    },
    PhaseChanged {
        round: RoundNo,
        phase: Phase,
    },
    /// The lead's validated output: its operations apply in order.
    LeadAccepted {
        round: RoundNo,
        output: LeadOutput,
    },
    /// The user holds the review after each consolidated round (`true`), or
    /// lets rounds follow each other again.
    HoldSet {
        #[serde(default)]
        hold: bool,
    },
    /// The user raised the round limit to `max_rounds`. A review that ended
    /// at its limit is open again.
    RoundsExtended {
        max_rounds: RoundNo,
        #[serde(default)]
        at: Millis,
    },
    SessionClosed,
}

/// The log does not fold: an event names something that is not there, or
/// breaks the order the engine writes in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReduceError(pub String);

impl fmt::Display for ReduceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReduceError {}

fn err<T>(message: impl Into<String>) -> Result<T, ReduceError> {
    Err(ReduceError(message.into()))
}

pub fn create(batch: &[Event]) -> Result<Session, ReduceError> {
    let Some((
        Event::Created {
            id,
            request,
            target,
            manifest,
            workdir: context,
            max_rounds,
            lead,
            reviewers,
            at,
        },
        rest,
    )) = batch.split_first()
    else {
        return err("the log does not start with a created event");
    };
    for (name, sha) in [
        ("base", &manifest.base_sha),
        ("head", &manifest.head_sha),
        ("merge-base", &manifest.merge_base),
    ] {
        if let Some(sha) = sha
            && !is_object_id(sha)
        {
            return err(format!("the {name} commit {sha:?} is not a git object id"));
        }
    }
    let mut session = Session {
        id: id.clone(),
        request: request.clone(),
        target: (**target).clone(),
        manifest: (**manifest).clone(),
        workdir: context.as_ref().and_then(|c| c.workdir.clone()),
        auto: context.as_ref().and_then(|c| c.auto.clone()),
        max_rounds: *max_rounds,
        rev: 1,
        created_at: *at,
        lead: lead.clone(),
        reviewers: reviewers.clone(),
        raw: Vec::new(),
        findings: Vec::new(),
        dismissed: Vec::new(),
        coverage: BTreeMap::new(),
        messages: Vec::new(),
        rounds: Vec::new(),
        closed: false,
        hold: false,
        verdict_round: None,
    };
    for event in rest {
        apply_one(&mut session, event)?;
    }
    Ok(session)
}

/// Applies one batch and bumps `rev`. On an error the session is left part
/// way through the batch and must be dropped.
pub fn apply(session: &mut Session, batch: &[Event]) -> Result<(), ReduceError> {
    session.rev += 1;
    for event in batch {
        apply_one(session, event)?;
    }
    Ok(())
}

pub fn replay<I>(batches: I) -> Result<Session, ReduceError>
where
    I: IntoIterator,
    I::Item: AsRef<[Event]>,
{
    let mut batches = batches.into_iter();
    let Some(first) = batches.next() else {
        return err("the log is empty");
    };
    let mut session = create(first.as_ref())?;
    for batch in batches {
        apply(&mut session, batch.as_ref())?;
    }
    Ok(session)
}

fn round_mut(session: &mut Session, no: RoundNo) -> Result<&mut Round, ReduceError> {
    match session.round_mut(no) {
        Some(round) => Ok(round),
        None => err(format!("no round {no}")),
    }
}

fn receipt_mut<'a>(
    session: &'a mut Session,
    round: RoundNo,
    to: &PartId,
) -> Result<&'a mut Receipt, ReduceError> {
    match round_mut(session, round)?.brief_mut(to) {
        Some(brief) => Ok(&mut brief.receipt),
        None => err(format!("no brief to {to} in round {round}")),
    }
}

fn finding_mut<'a>(session: &'a mut Session, id: &str) -> Result<&'a mut Finding, ReduceError> {
    match session.finding_mut(id) {
        Some(finding) => Ok(finding),
        None => err(format!("no finding {id}")),
    }
}

/// Marks a finding withdrawn once every reviewer who raised it withdrew.
fn settle_withdrawal(finding: &mut Finding, round: RoundNo) {
    if !finding.status.is_live() {
        return;
    }
    let origins: Vec<PartId> = finding.origins().into_iter().cloned().collect();
    let all_withdrew = !origins.is_empty()
        && origins.iter().all(|part| {
            finding
                .latest_stance(part)
                .is_some_and(|s| s.kind == StanceKind::Withdraw)
        });
    if all_withdrew {
        finding.status = FindingStatus::Withdrawn;
        finding.record(round);
    }
}

/// Moves `from` (a raw key or another finding's id) into finding `into`.
fn absorb(
    session: &mut Session,
    into: &str,
    from: &str,
    round: RoundNo,
) -> Result<(), ReduceError> {
    if let Some(key) = RawKey::parse(from) {
        if session.raw_finding(&key).is_none() {
            return err(format!("no raw finding {key}"));
        }
        if session.placement(&key).is_some() {
            return err(format!("raw finding {key} is already placed"));
        }
        finding_mut(session, into)?.sources.push(key);
        return Ok(());
    }
    if from == into {
        return err(format!("finding {into} cannot be merged into itself"));
    }
    let merged = finding_mut(session, from)?;
    let sources = std::mem::take(&mut merged.sources);
    let stances = std::mem::take(&mut merged.stances);
    merged.status = FindingStatus::Merged;
    merged.merged_into = Some(into.to_owned());
    merged.record(round);
    // The merged finding keeps nothing of its own, so nothing is counted
    // twice; its thread lives on in the finding it went into.
    let target = finding_mut(session, into)?;
    for key in sources {
        if !target.sources.contains(&key) {
            target.sources.push(key);
        }
    }
    for (part, mut list) in stances {
        let entry = target.stances.entry(part).or_default();
        entry.append(&mut list);
        entry.sort_by_key(|s| s.round);
    }
    Ok(())
}

fn apply_lead(
    session: &mut Session,
    round: RoundNo,
    output: &LeadOutput,
) -> Result<(), ReduceError> {
    let rev = session.rev;
    for op in &output.ops {
        match op {
            LeadOp::Add {
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
                if session.finding(id).is_some() {
                    return err(format!("finding {id} already exists"));
                }
                let (in_target, in_diff) = match location {
                    Some(loc) => super::validate::locate(&session.manifest, loc),
                    None => (true, None),
                };
                let mut finding = Finding {
                    id: id.clone(),
                    claim: claim.clone(),
                    location: location.clone(),
                    in_target,
                    in_diff,
                    severity: *severity,
                    category: category.clone(),
                    confidence: *confidence,
                    status: *status,
                    merged_into: None,
                    rationale: rationale.clone(),
                    fix: fix.clone(),
                    sources: Vec::new(),
                    opened_in: round,
                    changed_rev: rev,
                    stances: BTreeMap::new(),
                    history: Vec::new(),
                };
                finding.record(round);
                session.findings.push(finding);
                for item in from {
                    absorb(session, id, item, round)?;
                }
            }
            LeadOp::Merge { id, from } => {
                for item in from {
                    absorb(session, id, item, round)?;
                }
                finding_mut(session, id)?.changed_rev = rev;
            }
            LeadOp::Update {
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
                let located = location
                    .as_ref()
                    .map(|loc| super::validate::locate(&session.manifest, loc));
                let finding = finding_mut(session, id)?;
                if let Some(claim) = claim {
                    finding.claim = claim.clone();
                }
                if let (Some(loc), Some((in_target, in_diff))) = (location, located) {
                    finding.location = Some(loc.clone());
                    finding.in_target = in_target;
                    finding.in_diff = in_diff;
                }
                if let Some(severity) = severity {
                    finding.severity = *severity;
                }
                if let Some(category) = category {
                    finding.category = category.clone();
                }
                if let Some(confidence) = confidence {
                    finding.confidence = *confidence;
                }
                if let Some(status) = status {
                    finding.status = *status;
                }
                if let Some(rationale) = rationale {
                    finding.rationale = rationale.clone();
                }
                if let Some(fix) = fix {
                    finding.fix = Some(fix.clone());
                }
                finding.changed_rev = rev;
                finding.record(round);
            }
            LeadOp::Dismiss { raw, reason } => {
                if session.raw_finding(raw).is_none() {
                    return err(format!("no raw finding {raw} to dismiss"));
                }
                if session.placement(raw).is_some() {
                    return err(format!("raw finding {raw} is already placed"));
                }
                session.dismissed.push(super::types::Dismissal {
                    raw: raw.clone(),
                    round,
                    reason: reason.clone(),
                });
            }
        }
    }
    round_mut(session, round)?.outcome = Some(output.clone());
    Ok(())
}

fn apply_one(session: &mut Session, event: &Event) -> Result<(), ReduceError> {
    match event {
        Event::Created { .. } => return err("a second created event"),
        Event::PaneAssigned { part, pane_id } => {
            let slot = std::iter::once(&mut session.lead)
                .chain(&mut session.reviewers)
                .find(|p| &p.id == part);
            match slot {
                Some(participant) => participant.pane_id = Some(*pane_id),
                None => return err(format!("no participant {part}")),
            }
        }
        Event::RoundStarted { no, kind, at } => {
            let expected = session.rounds.len() as RoundNo + 1;
            if *no != expected {
                return err(format!("round {no} started, expected round {expected}"));
            }
            session.rounds.push(Round {
                no: *no,
                kind: *kind,
                phase: Phase::Briefing,
                started_at: *at,
                briefs: Vec::new(),
                reviews: BTreeMap::new(),
                lead_brief: None,
                outcome: None,
                verdict_post: None,
            });
        }
        Event::MessagePosted { message } => {
            session.messages.push(message.clone());
        }
        Event::BriefIssued { round, brief } => {
            let is_lead = brief.to == session.lead.id;
            let r = round_mut(session, *round)?;
            if is_lead {
                r.lead_brief = Some(brief.clone());
            } else {
                r.briefs.push(brief.clone());
            }
        }
        Event::BriefSent { round, to, at } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.sent = Some(*at);
            receipt.failed = None;
        }
        Event::BriefStarted { round, to, at } => {
            let receipt = receipt_mut(session, *round, to)?;
            if receipt.started.is_none() {
                receipt.started = Some(*at);
            }
        }
        Event::BriefRetried { round, to, .. } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.retries = receipt.retries.saturating_add(1);
            receipt.sent = None;
            receipt.started = None;
            receipt.failed = None;
            // A retry sends the whole brief: the agent may be a new one that
            // never saw the output a correction would refer to.
            receipt.rejected = None;
            receipt.rejected_digest = None;
        }
        Event::BriefFailed { round, to, reason } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.failed = Some(reason.clone());
        }
        Event::BriefReset { round, to } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.sent = None;
            receipt.started = None;
            receipt.retries = 0;
            receipt.failed = None;
            receipt.rejected = None;
            receipt.rejected_digest = None;
        }
        Event::ReplyRejected {
            round,
            to,
            error,
            digest,
        } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.retries = receipt.retries.saturating_add(1);
            receipt.sent = None;
            receipt.started = None;
            receipt.rejected = Some(error.clone());
            receipt.rejected_digest = *digest;
        }
        Event::ReplyAccepted { round, to, at } => {
            let receipt = receipt_mut(session, *round, to)?;
            receipt.replied = Some(*at);
            // A reply proves the brief arrived, even when the `Sent` was lost
            // with the link.
            receipt.sent.get_or_insert(*at);
            receipt.rejected = None;
            receipt.rejected_digest = None;
            receipt.failed = None;
        }
        Event::ReviewRecorded {
            round,
            part,
            summary,
            findings,
            stances,
            coverage,
            at,
        } => {
            if !session.is_reviewer(part) {
                return err(format!("{part} is not a reviewer"));
            }
            for raw in findings {
                if session.raw_finding(&raw.key).is_some() {
                    return err(format!("raw finding {} recorded twice", raw.key));
                }
                session.raw.push(raw.clone());
            }
            for (id, stance) in stances {
                let finding = finding_mut(session, id)?;
                finding
                    .stances
                    .entry(part.clone())
                    .or_default()
                    .push(stance.clone());
                settle_withdrawal(finding, *round);
            }
            if let Some(coverage) = coverage {
                let entry = session.coverage.entry(part.clone()).or_default();
                for path in &coverage.reviewed {
                    if !entry.reviewed.contains(path) {
                        entry.reviewed.push(path.clone());
                    }
                }
                entry.skipped = coverage
                    .skipped
                    .iter()
                    .filter(|(path, _)| !entry.reviewed.contains(path))
                    .cloned()
                    .collect();
            }
            round_mut(session, *round)?.reviews.insert(
                part.clone(),
                ReviewNote {
                    summary: summary.clone(),
                    findings: findings.len(),
                    stances: stances.len(),
                    at: *at,
                },
            );
        }
        Event::PhaseChanged { round, phase } => {
            round_mut(session, *round)?.phase = *phase;
        }
        Event::LeadAccepted { round, output } => {
            apply_lead(session, *round, output)?;
            if output.verdict.is_some() {
                session.verdict_round = Some(*round);
            }
        }
        Event::HoldSet { hold } => session.hold = *hold,
        Event::RoundsExtended { max_rounds, .. } => {
            if let Err(why) = session.extendable() {
                return err(format!("rounds extended where none can be: {why}"));
            }
            let base = session.max_rounds.max(session.rounds.len() as RoundNo);
            if *max_rounds <= base {
                return err(format!(
                    "round limit {max_rounds} is not above {base} after round {}",
                    session.rounds.len()
                ));
            }
            session.max_rounds = *max_rounds;
            session.closed = false;
            // The verdict was for the old limit: the review goes on.
            session.verdict_round = None;
        }
        Event::SessionClosed => {
            session.closed = true;
            if let Some(no) = session.verdict_round
                && let Some(post) = crate::timeline::verdict_post(session, no)
            {
                round_mut(session, no)?.verdict_post = Some(Box::new(post));
            }
        }
    }
    Ok(())
}
