//! The event log's fold: a session is `create` of the first batch, then
//! `apply` of every later one. Each batch bumps `rev` once. The events
//! themselves are in `types` (frozen with the other shared types).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub use crate::model::types::Event;
use crate::model::types::{
    Agreement, Attachment, BriefKind, CRITERIA_ROUND, CriteriaEdit, Criterion, CriterionResult,
    CriterionSource, CrossOutcome, Defect, DefectOp, DefectStatus, Dismissal, Evidence,
    EvidenceBody, EvidenceKey, JudgeOutput, Outcome, PartId, Phase, RawDefect, RawKey, Receipt,
    ReplyNote, Round, RoundNo, Session, Subject, Tally, is_object_id, natural_cmp,
};

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

/// The session the first batch creates (it starts with `Created`).
pub fn create(batch: &[Event]) -> Result<Session, ReduceError> {
    let Some((
        Event::Created {
            id,
            request,
            target,
            manifest,
            rebuild,
            isolation,
            spec,
            user_criteria,
            workdir,
            main_worktree,
            max_rounds,
            judge,
            verifiers,
            confirm,
            at,
            auto,
        },
        rest,
    )) = batch.split_first()
    else {
        return err("the log does not start with a created event");
    };
    for (name, sha) in [
        ("base", manifest.base_sha.as_ref()),
        ("head", manifest.head_sha.as_ref()),
        ("merge-base", manifest.merge_base.as_ref()),
        (
            "rebuild head",
            Some(&rebuild.head_sha).filter(|s| !s.is_empty()),
        ),
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
        rebuild: rebuild.clone(),
        isolation: *isolation,
        spec: spec.clone(),
        user_criteria: user_criteria.clone(),
        workdir: workdir.clone(),
        main_worktree: main_worktree.clone(),
        max_rounds: *max_rounds,
        rev: 1,
        created_at: *at,
        judge: judge.clone(),
        verifiers: verifiers.clone(),
        confirm: *confirm,
        auto: auto.clone(),
        recheck: None,
        criteria_brief: None,
        criteria_accepted: false,
        criteria_summary: String::new(),
        criteria: Vec::new(),
        criteria_edits: Vec::new(),
        removed_criteria: Vec::new(),
        frozen_at: None,
        worktrees: BTreeMap::new(),
        results: Vec::new(),
        evidence: Vec::new(),
        cross_checks: Vec::new(),
        raw_defects: Vec::new(),
        defects: Vec::new(),
        dismissed: Vec::new(),
        rulings: BTreeMap::new(),
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

/// The session a whole log folds into.
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

/// The receipt of `to`'s brief of `kind` in `round` (round
/// [`CRITERIA_ROUND`]: the judge's criteria brief).
fn receipt_mut<'a>(
    session: &'a mut Session,
    round: RoundNo,
    to: &PartId,
    kind: BriefKind,
) -> Result<&'a mut Receipt, ReduceError> {
    if round == CRITERIA_ROUND {
        return match &mut session.criteria_brief {
            Some(brief) if &brief.to == to && kind == BriefKind::Work => Ok(&mut brief.receipt),
            _ => err(format!("no criteria brief to {to}")),
        };
    }
    match round_mut(session, round)?.brief_mut(to, kind) {
        Some(brief) => Ok(&mut brief.receipt),
        None => err(format!("no {kind:?} brief to {to} in round {round}")),
    }
}

/// The receipt a `ReplyAccepted` closes: the work brief while it waits for
/// its reply; once that is taken in, the re-run brief (whose "reply" is
/// every item having an outcome, L1-5).
fn accepted_receipt<'a>(
    session: &'a mut Session,
    round: RoundNo,
    to: &PartId,
) -> Result<&'a mut Receipt, ReduceError> {
    let work_open = round == CRITERIA_ROUND
        || session
            .round(round)
            .and_then(|r| r.brief(to, BriefKind::Work))
            .is_none_or(|b| b.receipt.replied.is_none());
    let kind = if work_open {
        BriefKind::Work
    } else {
        BriefKind::Rerun
    };
    receipt_mut(session, round, to, kind)
}

fn defect_mut<'a>(session: &'a mut Session, id: &str) -> Result<&'a mut Defect, ReduceError> {
    match session.defect_mut(id) {
        Some(defect) => Ok(defect),
        None => err(format!("no defect {id}")),
    }
}

/// Moves `from` (a raw key or another defect's id) into defect `into`.
fn absorb(
    session: &mut Session,
    into: &str,
    from: &str,
    round: RoundNo,
) -> Result<(), ReduceError> {
    if let Some(key) = RawKey::parse(from) {
        if session.raw_defect(&key).is_none() {
            return err(format!("no raw defect {key}"));
        }
        if session.placement(&key).is_some() {
            return err(format!("raw defect {key} is already placed"));
        }
        defect_mut(session, into)?.sources.push(key);
        return Ok(());
    }
    if from == into {
        return err(format!("defect {into} cannot be merged into itself"));
    }
    let merged = defect_mut(session, from)?;
    let sources = std::mem::take(&mut merged.sources);
    merged.status = DefectStatus::Merged;
    merged.merged_into = Some(into.to_owned());
    merged.record(round);
    // The merged defect keeps no sources of its own, so nothing is counted
    // twice; its history lives on in the defect it went into.
    let target = defect_mut(session, into)?;
    for key in sources {
        if !target.sources.contains(&key) {
            target.sources.push(key);
        }
    }
    Ok(())
}

fn apply_judge(
    session: &mut Session,
    round: RoundNo,
    output: &JudgeOutput,
) -> Result<(), ReduceError> {
    let rev = session.rev;
    for (criterion, ruling) in &output.rulings {
        if session.criterion(criterion).is_none() {
            return err(format!("a ruling on unknown criterion {criterion}"));
        }
        session
            .rulings
            .entry(criterion.clone())
            .or_default()
            .push(ruling.clone());
    }
    for op in &output.defect_ops {
        match op {
            DefectOp::Add {
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
                if session.defect(id).is_some() {
                    return err(format!("defect {id} already exists"));
                }
                let (in_target, in_diff) = match location {
                    Some(loc) => super::validate::locate(session, loc),
                    None => (true, None),
                };
                let mut defect = Defect {
                    id: id.clone(),
                    claim: claim.clone(),
                    repro: repro.clone(),
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
                    history: Vec::new(),
                };
                defect.record(round);
                session.defects.push(defect);
                for item in from {
                    absorb(session, id, item, round)?;
                }
            }
            DefectOp::Merge { id, from } => {
                for item in from {
                    absorb(session, id, item, round)?;
                }
                defect_mut(session, id)?.changed_rev = rev;
            }
            DefectOp::Update {
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
                let located = location
                    .as_ref()
                    .map(|loc| super::validate::locate(session, loc));
                let defect = defect_mut(session, id)?;
                if let Some(claim) = claim {
                    defect.claim = claim.clone();
                }
                if let Some(repro) = repro {
                    defect.repro = repro.clone();
                }
                if let (Some(loc), Some((in_target, in_diff))) = (location, located) {
                    defect.location = Some(loc.clone());
                    defect.in_target = in_target;
                    defect.in_diff = in_diff;
                }
                if let Some(severity) = severity {
                    defect.severity = *severity;
                }
                if let Some(category) = category {
                    defect.category = category.clone();
                }
                if let Some(confidence) = confidence {
                    defect.confidence = *confidence;
                }
                if let Some(status) = status {
                    defect.status = *status;
                }
                if let Some(rationale) = rationale {
                    defect.rationale = rationale.clone();
                }
                if let Some(fix) = fix {
                    defect.fix = Some(fix.clone());
                }
                defect.changed_rev = rev;
                defect.record(round);
            }
            DefectOp::Dismiss { raw, reason } => {
                if session.raw_defect(raw).is_none() {
                    return err(format!("no raw defect {raw} to dismiss"));
                }
                if session.placement(raw).is_some() {
                    return err(format!("raw defect {raw} is already placed"));
                }
                session.dismissed.push(Dismissal {
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

/// The next `AC-<n>` the person's added criterion gets: one past every
/// number used, removed criteria included, so a removed id is never
/// reused.
fn next_criterion_id(session: &Session) -> String {
    let max = session
        .criteria
        .iter()
        .chain(&session.removed_criteria)
        .filter_map(|c| c.id.strip_prefix("AC-")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("AC-{}", max + 1)
}

fn apply_edit(session: &mut Session, edit: &CriteriaEdit) -> Result<(), ReduceError> {
    let position = |session: &Session, id: &str| {
        session
            .criteria
            .iter()
            .position(|c| c.id == id)
            .ok_or_else(|| ReduceError(format!("no criterion {id}")))
    };
    match edit {
        CriteriaEdit::Add {
            statement,
            how,
            priority,
        } => {
            let id = next_criterion_id(session);
            session.criteria.push(Criterion {
                id,
                statement: statement.clone(),
                how: how.clone(),
                priority: *priority,
                source: CriterionSource::User,
                edited: true,
            });
        }
        CriteriaEdit::Edit {
            id,
            statement,
            how,
            priority,
        } => {
            let at = position(session, id)?;
            let criterion = &mut session.criteria[at];
            if let Some(statement) = statement {
                criterion.statement = statement.clone();
            }
            if let Some(how) = how {
                criterion.how = how.clone();
            }
            if let Some(priority) = priority {
                criterion.priority = *priority;
            }
            criterion.edited = true;
        }
        CriteriaEdit::Remove { id } => {
            let at = position(session, id)?;
            let removed = session.criteria.remove(at);
            session.removed_criteria.push(removed);
        }
        CriteriaEdit::Move { id, to } => {
            let at = position(session, id)?;
            if *to >= session.criteria.len() {
                return err(format!("criterion {id} moved past the end ({to})"));
            }
            let moved = session.criteria.remove(at);
            session.criteria.insert(*to, moved);
        }
    }
    Ok(())
}

fn apply_one(session: &mut Session, event: &Event) -> Result<(), ReduceError> {
    match event {
        Event::Created { .. } => return err("a second created event"),
        Event::PaneAssigned { part, pane_id } => {
            let slot = std::iter::once(&mut session.judge)
                .chain(&mut session.verifiers)
                .find(|p| &p.id == part);
            match slot {
                Some(participant) => participant.pane_id = Some(*pane_id),
                None => return err(format!("no participant {part}")),
            }
        }
        Event::BriefIssued { round, brief } => {
            if *round == CRITERIA_ROUND {
                if brief.to != session.judge.id || brief.kind != BriefKind::Work {
                    return err(format!("a criteria brief to {}", brief.to));
                }
                session.criteria_brief = Some(brief.clone());
                return Ok(());
            }
            let is_judge = brief.to == session.judge.id;
            if !is_judge && !session.is_verifier(&brief.to) {
                return err(format!("a brief to unknown participant {}", brief.to));
            }
            let r = round_mut(session, *round)?;
            if r.brief(&brief.to, brief.kind).is_some() {
                return err(format!(
                    "a second {:?} brief to {} in round {round}",
                    brief.kind, brief.to
                ));
            }
            match (is_judge, brief.kind) {
                (true, BriefKind::Work) => r.judge_brief = Some(brief.clone()),
                (true, BriefKind::Rerun) => return err("a re-run brief to the judge"),
                (false, BriefKind::Work) => r.briefs.push(brief.clone()),
                (false, BriefKind::Rerun) => r.rerun_briefs.push(brief.clone()),
            }
        }
        Event::BriefSent {
            round,
            to,
            kind,
            at,
        } => {
            let receipt = receipt_mut(session, *round, to, *kind)?;
            receipt.sent = Some(*at);
            receipt.failed = None;
        }
        Event::BriefStarted {
            round,
            to,
            kind,
            at,
        } => {
            let receipt = receipt_mut(session, *round, to, *kind)?;
            if receipt.started.is_none() {
                receipt.started = Some(*at);
            }
        }
        Event::BriefRetried {
            round, to, kind, ..
        } => {
            let receipt = receipt_mut(session, *round, to, *kind)?;
            receipt.retries = receipt.retries.saturating_add(1);
            receipt.sent = None;
            receipt.started = None;
            receipt.failed = None;
            // A retry sends the whole brief: the agent may be a new one that
            // never saw the output a correction would refer to.
            receipt.rejected = None;
            receipt.rejected_digest = None;
        }
        Event::BriefFailed {
            round,
            to,
            kind,
            reason,
        } => {
            let receipt = receipt_mut(session, *round, to, *kind)?;
            receipt.failed = Some(reason.clone());
        }
        Event::BriefReset { round, to, kind } => {
            let receipt = receipt_mut(session, *round, to, *kind)?;
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
            let receipt = receipt_mut(session, *round, to, BriefKind::Work)?;
            receipt.retries = receipt.retries.saturating_add(1);
            receipt.sent = None;
            receipt.started = None;
            receipt.rejected = Some(error.clone());
            receipt.rejected_digest = *digest;
        }
        Event::ReplyAccepted { round, to, at } => {
            let receipt = accepted_receipt(session, *round, to)?;
            receipt.replied = Some(*at);
            // A reply proves the brief arrived, even when the `Sent` was lost
            // with the link.
            receipt.sent.get_or_insert(*at);
            receipt.rejected = None;
            receipt.rejected_digest = None;
            receipt.failed = None;
        }
        Event::CriteriaAccepted { summary, criteria } => {
            if session.frozen_at.is_some() {
                return err("criteria accepted after they were frozen");
            }
            session.criteria_accepted = true;
            session.criteria_summary = summary.clone();
            session.criteria = criteria.clone();
        }
        Event::CriteriaReused { previous } => {
            if session.recheck.is_some()
                || session.criteria_accepted
                || session.criteria_brief.is_some()
                || session.frozen_at.is_some()
                || !session.rounds.is_empty()
                || previous.parent == session.id
            {
                return err("criteria reused outside a new verification");
            }
            previous.check().map_err(ReduceError)?;
            session.confirm = true;
            session.criteria_accepted = true;
            session.criteria_summary = format!(
                "{} の基準を引き継ぎました。今回の対象に合うか確認してください。出典は基準が作られたときの対象・仕様を指します。",
                previous.parent
            );
            session.criteria = previous.criteria.clone();
            session.removed_criteria = previous.removed_criteria.clone();
            session.user_criteria = previous.user_criteria.clone();
            session.recheck = Some((**previous).clone());
        }
        Event::CriteriaEdited { edit, at } => {
            if !session.criteria_accepted || session.frozen_at.is_some() {
                return err("criteria edited outside their confirmation");
            }
            apply_edit(session, edit)?;
            session.criteria_edits.push((edit.clone(), *at));
        }
        Event::CriteriaFrozen { at } => {
            if !session.criteria_accepted {
                return err("criteria frozen before any were accepted");
            }
            session.frozen_at = Some(*at);
        }
        Event::WorktreeMade { part, path } => {
            if !session.is_verifier(part) {
                return err(format!("a worktree for unknown verifier {part}"));
            }
            session.worktrees.insert(
                part.clone(),
                crate::model::types::Worktree {
                    path: path.clone(),
                    state: crate::model::types::WorktreeState::Made,
                },
            );
        }
        Event::WorktreeFailed { part, path, reason } => {
            if !session.is_verifier(part) {
                return err(format!("a worktree for unknown verifier {part}"));
            }
            session.worktrees.insert(
                part.clone(),
                crate::model::types::Worktree {
                    path: path.clone(),
                    state: crate::model::types::WorktreeState::Failed {
                        reason: reason.clone(),
                    },
                },
            );
        }
        Event::WorktreeRemoved { part } => match session.worktrees.get_mut(part) {
            Some(tree) => tree.state = crate::model::types::WorktreeState::Removed,
            None => return err(format!("no worktree of {part} to remove")),
        },
        Event::RoundStarted {
            no,
            kind,
            assignments,
            at,
        } => {
            let expected = session.rounds.len() as RoundNo + 1;
            if *no != expected {
                return err(format!("round {no} started, expected round {expected}"));
            }
            if session.frozen_at.is_none() {
                return err("a round started before the criteria were frozen");
            }
            session.rounds.push(Round {
                no: *no,
                kind: *kind,
                phase: Phase::Briefing,
                started_at: *at,
                briefs: Vec::new(),
                rerun_briefs: Vec::new(),
                judge_brief: None,
                replies: BTreeMap::new(),
                ingested: BTreeSet::new(),
                reruns: BTreeMap::new(),
                tails: BTreeMap::new(),
                assignments: assignments.clone(),
                outcome: None,
                verdict_post: None,
            });
        }
        Event::MessagePosted { message } => session.messages.push(message.clone()),
        Event::ReplyRecorded {
            round,
            part,
            summary,
            environment,
            results,
            evidence,
            defects,
            cross_checks,
            at,
        } => {
            if !session.is_verifier(part) {
                return err(format!("{part} is not a verifier"));
            }
            for item in evidence {
                if session.evidence_item(&item.key).is_some() {
                    return err(format!("evidence {} recorded twice", item.key));
                }
                session.evidence.push(item.clone());
            }
            for raw in defects {
                if session.raw_defect(&raw.key).is_some() {
                    return err(format!("raw defect {} recorded twice", raw.key));
                }
                session.raw_defects.push(raw.clone());
            }
            session.results.extend(results.iter().cloned());
            session.cross_checks.extend(cross_checks.iter().cloned());
            round_mut(session, *round)?.replies.insert(
                part.clone(),
                ReplyNote {
                    summary: summary.clone(),
                    environment: environment.clone(),
                    results: results.len(),
                    evidence: evidence.len(),
                    defects: defects.len(),
                    cross_checks: cross_checks.len(),
                    at: *at,
                },
            );
        }
        Event::EvidenceIngested {
            round,
            part,
            items,
            tails,
        } => {
            for (key, stored) in items {
                match session.evidence_item_mut(key) {
                    Some(item) => item.stored = Some(stored.clone()),
                    None => return err(format!("no evidence {key} to store")),
                }
            }
            let r = round_mut(session, *round)?;
            r.ingested.insert(part.clone());
            r.tails
                .extend(tails.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        Event::RerunRequested {
            round,
            part,
            nonce,
            items,
            script_digest,
        } => {
            round_mut(session, *round)?.reruns.insert(
                part.clone(),
                crate::model::types::RerunRequest {
                    nonce: nonce.clone(),
                    items: items.clone(),
                    script_digest: *script_digest,
                },
            );
        }
        Event::RerunFinished { key, rerun } => match session.evidence_item_mut(key) {
            Some(item) => item.rerun = Some(rerun.clone()),
            None => return err(format!("no evidence {key} for a re-run")),
        },
        Event::EvidenceAttached {
            key,
            round,
            body,
            stored,
            shows,
            at,
        } => {
            if !key.is_user() {
                return err(format!("attached evidence {key} is not a person's key"));
            }
            if session.evidence_item(key).is_some() {
                return err(format!("evidence {key} recorded twice"));
            }
            let mut body = body.clone();
            if let EvidenceBody::File { shows: own, .. } = &mut body {
                own.clone_from(shows);
            }
            session.evidence.push(Evidence {
                key: key.clone(),
                body,
                stored: Some(stored.clone()),
                rerun: None,
                attached: Some(Attachment {
                    round: *round,
                    at: *at,
                }),
            });
        }
        Event::PhaseChanged { round, phase } => {
            round_mut(session, *round)?.phase = *phase;
        }
        Event::JudgeAccepted { round, output } => {
            apply_judge(session, *round, output)?;
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
            // The verdict was for the old limit: the verification goes on.
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

impl Session {
    /// Whether the verification waits for the person to start its next
    /// round.
    pub fn waits_to_continue(&self) -> bool {
        !self.closed
            && self.hold
            && self
                .current_round()
                .is_some_and(|round| round.phase == Phase::Closed)
    }

    /// Whether the person may add rounds, or why not. A running
    /// verification may get more rounds until its last round is being
    /// ruled on; an ended one only when it ended by running all its rounds.
    pub fn extendable(&self) -> Result<(), &'static str> {
        let at_limit = self.rounds.len() as RoundNo >= self.max_rounds;
        let phase = self.current_round().map(|round| round.phase);
        if !self.closed {
            return if at_limit && phase == Some(Phase::Ruling) {
                Err("最終ラウンドの判定中です（総合判定の後に追加できます）")
            } else {
                Ok(())
            };
        }
        if at_limit && phase == Some(Phase::Closed) {
            Ok(())
        } else {
            Err("ラウンド上限の前に終了した検証です")
        }
    }

    /// Whether the engine adds rounds now: [`Session::extendable`], and an
    /// ended verification's worktrees all removed first (closing asks for
    /// their removal, L1-3; one whose removal failed stays `Made`).
    pub fn extendable_now(&self) -> Result<(), &'static str> {
        self.extendable()?;
        if self.closed
            && self
                .worktrees
                .values()
                .any(|t| t.state == crate::model::types::WorktreeState::Made)
        {
            return Err(
                "ワークツリーの削除が終わっていません（削除が終わってから追加してください）",
            );
        }
        Ok(())
    }

    /// The defect (or dismissal) a raw defect went to, if any.
    pub fn placement(&self, key: &RawKey) -> Option<&str> {
        self.defects
            .iter()
            .find(|d| d.sources.contains(key))
            .map(|d| d.id.as_str())
            .or_else(|| {
                self.dismissed
                    .iter()
                    .any(|d| &d.raw == key)
                    .then_some("dismissed")
            })
    }

    /// The raw defects in no defect and not dismissed: those of a round the
    /// judge has not ruled on (yet, or ever).
    pub fn unplaced(&self) -> Vec<&RawDefect> {
        self.raw_defects
            .iter()
            .filter(|r| self.placement(&r.key).is_none())
            .collect()
    }

    /// The next free `D<n>` id.
    pub fn next_defect_id(&self) -> String {
        let mut n = self.defects.len() + 1;
        while self.defect(&format!("D{n}")).is_some() {
            n += 1;
        }
        format!("D{n}")
    }

    /// Every category a defect may take: the strategy ids on the panel's
    /// side (every built-in one, the panel's custom ones), then `other`.
    pub fn categories(&self) -> Vec<String> {
        let mut ids: Vec<String> = crate::model::types::BuiltinStrategy::ALL
            .iter()
            .map(|s| s.id().to_owned())
            .collect();
        for part in &self.verifiers {
            if let Some(crate::model::types::Strategy::Custom { id, .. }) = &part.strategy
                && !ids.contains(id)
            {
                ids.push(id.clone());
            }
        }
        ids.push("other".to_owned());
        ids
    }

    /// The label of a category: a strategy label, or その他.
    pub fn category_label(&self, id: &str) -> String {
        if let Some(strategy) = crate::model::types::BuiltinStrategy::parse(id) {
            return strategy.label().to_owned();
        }
        self.verifiers
            .iter()
            .filter_map(|p| p.strategy.as_ref())
            .find(|s| s.id() == id)
            .map(|s| s.label())
            .unwrap_or_else(|| "その他".to_owned())
    }

    /// `part`'s latest result on `criterion` (its round-1 result, or a
    /// later update).
    pub fn latest_result(&self, part: &PartId, criterion: &str) -> Option<&CriterionResult> {
        self.results
            .iter()
            .rev()
            .find(|r| &r.part == part && r.criterion == criterion)
    }

    /// `part`'s latest result on `criterion` as of the end of `round`.
    pub fn result_as_of(
        &self,
        part: &PartId,
        criterion: &str,
        round: RoundNo,
    ) -> Option<&CriterionResult> {
        self.results
            .iter()
            .rev()
            .find(|r| &r.part == part && r.criterion == criterion && r.round <= round)
    }

    /// The `command` items marked `rerun: true` in `part`'s reply of
    /// `round`, in reply order.
    pub fn rerun_items(&self, round: RoundNo, part: &PartId) -> Vec<&Evidence> {
        self.evidence
            .iter()
            .filter(|e| {
                e.key
                    .split()
                    .is_some_and(|(p, r, _)| &p == part && r == round)
                    && matches!(e.body, EvidenceBody::Command { rerun: true, .. })
            })
            .collect()
    }

    /// The items `part` reported in `round`.
    pub fn reported_items(&self, round: RoundNo, part: &PartId) -> Vec<&Evidence> {
        self.evidence
            .iter()
            .filter(|e| {
                e.key
                    .split()
                    .is_some_and(|(p, r, _)| &p == part && r == round)
            })
            .collect()
    }

    /// The judge's summary of the latest ruled round before `round`.
    pub fn summary_before(&self, round: RoundNo) -> Option<(RoundNo, &str)> {
        self.rounds
            .iter()
            .rev()
            .filter(|r| r.no < round)
            .find_map(|r| r.outcome.as_ref().map(|o| (r.no, o.summary.as_str())))
    }

    /// The verifiers counted for agreement: those not failed in the latest
    /// round that briefed them.
    fn counted_verifiers(&self) -> Vec<&PartId> {
        let latest = self.rounds.iter().rev().find(|r| !r.briefs.is_empty());
        self.verifier_ids()
            .filter(|part| {
                latest
                    .and_then(|r| r.brief(part, BriefKind::Work))
                    .is_none_or(|b| b.receipt.failed.is_none() || b.receipt.replied.is_some())
            })
            .collect()
    }

    /// `defect`'s id and the ids of every defect merged into it, directly
    /// or through another merged one: every id a cross-check counted with
    /// it names.
    pub(crate) fn merged_ids<'a>(&'a self, defect: &'a Defect) -> BTreeSet<&'a str> {
        let mut ids = BTreeSet::from([defect.id.as_str()]);
        loop {
            let before = ids.len();
            for d in &self.defects {
                if d.merged_into
                    .as_deref()
                    .is_some_and(|into| ids.contains(into))
                {
                    ids.insert(d.id.as_str());
                }
            }
            if ids.len() == before {
                return ids;
            }
        }
    }

    /// Who supports and disputes `defect`, from its sources and the
    /// cross-checks on it and on the defects merged into it (review L2-4):
    /// a merge moves the sources, and the checks on them count with them.
    pub fn tally(&self, defect: &Defect) -> Tally {
        let origins = defect.origins();
        let ids = self.merged_ids(defect);
        let mut supporters = Vec::new();
        let mut disputers = Vec::new();
        let mut silent = Vec::new();
        for part in self.counted_verifiers() {
            // The latest cross-check `part` made on the defect, if any.
            let check = self
                .cross_checks
                .iter()
                .rev()
                .find(|c| {
                    &c.by == part
                        && matches!(&c.subject, Subject::Defect { id } if ids.contains(id.as_str()))
                })
                .map(|c| c.outcome);
            match check {
                Some(CrossOutcome::Reproduced) => supporters.push(part.clone()),
                Some(CrossOutcome::NotReproduced | CrossOutcome::Disputed) => {
                    disputers.push(part.clone())
                }
                None if origins.contains(part) => supporters.push(part.clone()),
                None => silent.push(part.clone()),
            }
        }
        let agreement = if !disputers.is_empty() && disputers.len() >= supporters.len() {
            Agreement::Split
        } else if disputers.is_empty() && silent.is_empty() && supporters.len() > 1 {
            Agreement::Unanimous
        } else if disputers.is_empty() && supporters.len() <= 1 {
            Agreement::Single
        } else {
            Agreement::Majority
        };
        Tally {
            supporters,
            disputers,
            silent,
            agreement,
        }
    }

    /// Defects ranked (review L2-4): severity, status, confidence,
    /// supporters, opening round, then their id (D2 before D10).
    pub fn ranked(&self) -> Vec<&Defect> {
        let mut out: Vec<(&Defect, usize)> = self
            .defects
            .iter()
            .map(|d| (d, self.tally(d).supporters.len()))
            .collect();
        out.sort_by(|(a, sa), (b, sb)| {
            a.severity
                .cmp(&b.severity)
                .then(a.status.rank().cmp(&b.status.rank()))
                .then(a.confidence.cmp(&b.confidence))
                .then(sb.cmp(sa))
                .then(a.opened_in.cmp(&b.opened_in))
                .then_with(|| natural_cmp(&a.id, &b.id))
        });
        out.into_iter().map(|(d, _)| d).collect()
    }

    /// Criteria per latest ruling: verified, failed, inconclusive, not
    /// applicable, unruled.
    pub fn ruling_counts(&self) -> [usize; 5] {
        use crate::model::types::RulingKind;
        let mut counts = [0; 5];
        for c in &self.criteria {
            let slot = match self.ruling(&c.id).map(|r| r.kind) {
                Some(RulingKind::Verified) => 0,
                Some(RulingKind::Failed) => 1,
                Some(RulingKind::Inconclusive) => 2,
                Some(RulingKind::NotApplicable) => 3,
                None => 4,
            };
            counts[slot] += 1;
        }
        counts
    }

    /// Whether `key` names an `observation` item.
    pub fn is_observation(&self, key: &EvidenceKey) -> bool {
        self.evidence_item(key)
            .is_some_and(|e| matches!(e.body, EvidenceBody::Observation { .. }))
    }

    /// The results whose latest state is `outcome` on `criterion`, one per
    /// verifier, in panel order.
    pub fn latest_with(&self, criterion: &str, outcome: Outcome) -> Vec<&CriterionResult> {
        self.verifier_ids()
            .filter_map(|part| self.latest_result(part, criterion))
            .filter(|r| r.outcome == outcome)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::{Confidence, Location, Severity};
    use crate::testkit::{self, Rig, p};

    fn raw(part: &str, id: &str, round: RoundNo) -> RawDefect {
        RawDefect {
            key: RawKey {
                part: p(part),
                id: id.into(),
            },
            round,
            location: Some(Location {
                path: "src/store.rs".into(),
                line: Some(3),
                end_line: None,
            }),
            in_target: true,
            in_diff: None,
            severity: Severity::High,
            category: "falsify".into(),
            confidence: Confidence::High,
            claim: format!("claim {id}"),
            repro: "1. run".into(),
            evidence: Vec::new(),
            fix: None,
        }
    }

    fn add(id: &str, from: &[&str]) -> DefectOp {
        DefectOp::Add {
            id: id.into(),
            from: from.iter().map(|s| s.to_string()).collect(),
            claim: format!("defect {id}"),
            repro: "1. run".into(),
            location: None,
            severity: Severity::High,
            category: "falsify".into(),
            confidence: Confidence::High,
            status: DefectStatus::Open,
            rationale: "seen".into(),
            fix: None,
        }
    }

    fn judged(
        session: &mut Session,
        round: RoundNo,
        ops: Vec<DefectOp>,
    ) -> Result<(), ReduceError> {
        apply(
            session,
            &[Event::JudgeAccepted {
                round,
                output: JudgeOutput {
                    summary: "s".into(),
                    rulings: BTreeMap::new(),
                    defect_ops: ops,
                    assign: BTreeMap::new(),
                    verdict: None,
                    close: false,
                },
            }],
        )
    }

    /// A session in round 1 with raw defects of three verifiers recorded.
    fn with_raws() -> Session {
        let mut rig = Rig::new(2);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        let mut session = rig.engine.session().clone();
        for (part, id) in [("claude", "1"), ("codex", "1"), ("claude-2", "7")] {
            session.raw_defects.push(raw(part, id, 1));
        }
        session
    }

    #[test]
    fn ac18_every_raw_defect_is_placed_once_and_merge_moves_sources() {
        let mut session = with_raws();
        judged(
            &mut session,
            1,
            vec![
                add("D1", &["claude/1"]),
                add("D2", &["codex/1"]),
                DefectOp::Merge {
                    id: "D1".into(),
                    from: vec!["D2".into()],
                },
                DefectOp::Dismiss {
                    raw: RawKey::parse("claude-2/7").unwrap(),
                    reason: "duplicate".into(),
                },
            ],
        )
        .unwrap();
        let d1 = session.defect("D1").unwrap();
        assert_eq!(
            d1.sources,
            vec![
                RawKey::parse("claude/1").unwrap(),
                RawKey::parse("codex/1").unwrap()
            ]
        );
        let d2 = session.defect("D2").unwrap();
        assert_eq!(d2.status, DefectStatus::Merged);
        assert_eq!(d2.merged_into.as_deref(), Some("D1"));
        assert!(d2.sources.is_empty(), "a merged defect keeps no sources");
        assert!(session.unplaced().is_empty());
        assert_eq!(
            session.placement(&RawKey::parse("claude-2/7").unwrap()),
            Some("dismissed")
        );
        // A raw defect placed twice does not fold.
        let mut again = session.clone();
        assert!(judged(&mut again, 1, vec![add("D3", &["claude/1"])]).is_err());
        assert!(judged(&mut session, 1, vec![add("D9", &["agy/1"])]).is_err());
    }

    #[test]
    fn ac18_cross_checks_support_and_dispute_defects_and_ranking_breaks_ties_by_id() {
        let mut session = with_raws();
        judged(
            &mut session,
            1,
            vec![add("D10", &["claude/1"]), add("D2", &["codex/1"])],
        )
        .unwrap();
        let tally = session.tally(session.defect("D10").unwrap());
        assert_eq!(tally.supporters, vec![p("claude")]);
        assert_eq!(tally.agreement, Agreement::Single);
        session.cross_checks.push(crate::model::types::CrossCheck {
            round: 2,
            by: p("codex"),
            subject: Subject::Defect { id: "D10".into() },
            outcome: CrossOutcome::NotReproduced,
            note: "passes".into(),
            evidence: Vec::new(),
        });
        let tally = session.tally(session.defect("D10").unwrap());
        assert_eq!(tally.disputers, vec![p("codex")]);
        assert_eq!(tally.agreement, Agreement::Split);
        let ids: Vec<&str> = session.ranked().iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["D2", "D10"], "D2 has one supporter, D10 is split");
    }

    #[test]
    fn ac18_a_merge_keeps_the_cross_checks_on_the_merged_defect_in_the_tally() {
        let mut session = with_raws();
        judged(
            &mut session,
            1,
            vec![add("D1", &["codex/1"]), add("D2", &["claude-2/7"])],
        )
        .unwrap();
        session.cross_checks.push(crate::model::types::CrossCheck {
            round: 2,
            by: p("claude"),
            subject: Subject::Defect { id: "D1".into() },
            outcome: CrossOutcome::Reproduced,
            note: "fails here too".into(),
            evidence: Vec::new(),
        });
        let tally = session.tally(session.defect("D1").unwrap());
        assert_eq!(tally.supporters, vec![p("claude"), p("codex")]);
        judged(
            &mut session,
            1,
            vec![DefectOp::Merge {
                id: "D2".into(),
                from: vec!["D1".into()],
            }],
        )
        .unwrap();
        let tally = session.tally(session.defect("D2").unwrap());
        assert_eq!(
            tally.supporters,
            vec![p("claude"), p("codex"), p("claude-2")],
            "claude's reproduction of D1 moves with D1's sources"
        );
        assert_eq!(tally.agreement, Agreement::Unanimous);
    }

    #[test]
    fn criteria_edits_fold_with_fresh_ids_and_keep_what_was_removed() {
        let mut rig = Rig::new(2);
        testkit::accept_default_criteria(&mut rig);
        let mut session = rig.engine.session().clone();
        let edit = |s: &mut Session, edit: CriteriaEdit| {
            apply(s, &[Event::CriteriaEdited { edit, at: 5 }])
        };
        edit(&mut session, CriteriaEdit::Remove { id: "AC-3".into() }).unwrap();
        edit(
            &mut session,
            CriteriaEdit::Add {
                statement: "Quits cleanly".into(),
                how: "⌘Q".into(),
                priority: crate::model::types::Priority::Should,
            },
        )
        .unwrap();
        edit(
            &mut session,
            CriteriaEdit::Move {
                id: "AC-4".into(),
                to: 0,
            },
        )
        .unwrap();
        let ids: Vec<&str> = session.criteria.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["AC-4", "AC-1", "AC-2"], "a removed id is not reused");
        assert_eq!(session.removed_criteria[0].id, "AC-3");
        assert!(session.criteria[0].edited);
        assert_eq!(session.criteria[0].source, CriterionSource::User);
        assert!(edit(&mut session, CriteriaEdit::Remove { id: "AC-9".into() }).is_err());
    }
}
