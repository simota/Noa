//! The evidence standard the widget holds the judge to (L1-6, Judge
//! ruling): what each ruling needs, the verdict's consistency with the
//! rulings, the default cross-check assignments (L1-4 item 4) and when
//! round 1 may close early (L1-4 item 5). Pure.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::types::{
    CrossCheck, CrossOutcome, Decision, Defect, Evidence, EvidenceBody, EvidenceKey, Outcome,
    PartId, Priority, Rerun, RoundNo, Ruling, RulingKind, Session, Severity, Subject,
    USER_EVIDENCE_OWNER, Verdict,
};

/// Whether `item` is a command or a file, as opposed to an observation.
fn is_command_or_file(item: &Evidence) -> bool {
    matches!(
        item.body,
        EvidenceBody::Command { .. } | EvidenceBody::File { .. }
    )
}

/// Whether `item` counts toward the standard as stored evidence: a command
/// or a file whose file the widget stored (at ingest, or attached). One
/// whose file was gone before ingest counts for nothing (L1-6).
fn is_backed(item: &Evidence) -> bool {
    is_command_or_file(item) && item.stored.is_some()
}

fn is_matched_command(item: &Evidence) -> bool {
    is_backed(item)
        && matches!(item.body, EvidenceBody::Command { .. })
        && matches!(item.rerun, Some(Rerun::Matched { .. }))
}

/// Whether `key` is evidence for `criterion` (L1-6): cited by a result on
/// it, or by a cross-check whose subject is a result on it, or a media
/// item whose `shows` names it. A media item with `shows` is evidence only
/// for the criteria it names (L1-5a), whoever cites it.
fn belongs(session: &Session, criterion: &str, key: &EvidenceKey) -> bool {
    if let Some(Evidence {
        body: EvidenceBody::File { shows, .. },
        ..
    }) = session.evidence_item(key)
        && !shows.is_empty()
    {
        return shows.iter().any(|s| s == criterion);
    }
    session
        .results
        .iter()
        .any(|r| r.criterion == criterion && r.evidence.contains(key))
        || session.cross_checks.iter().any(|c| {
            matches!(&c.subject, Subject::Result { criterion: on, .. } if on == criterion)
                && c.evidence.contains(key)
        })
}

/// The independent sources behind `criterion` being met that the ruling
/// cites in `cited`: each verifier whose latest result on it is `pass` and
/// whose stored command or file evidence for that result is cited, and the
/// person, when `cited` holds media they attached showing it (it counts
/// only beside a verifier's pass, since one source is never enough).
fn independent_passes(
    session: &Session,
    criterion: &str,
    cited: &[EvidenceKey],
) -> BTreeSet<String> {
    let mut sources: BTreeSet<String> = session
        .latest_with(criterion, Outcome::Pass)
        .into_iter()
        .filter(|r| {
            r.evidence
                .iter()
                .any(|k| cited.contains(k) && session.evidence_item(k).is_some_and(is_backed))
        })
        .map(|r| r.part.to_string())
        .collect();
    let attached = cited.iter().filter(|k| k.is_user()).any(|k| {
        session.evidence_item(k).is_some_and(|e| {
            is_backed(e)
                && matches!(&e.body, EvidenceBody::File { shows, .. } if shows.iter().any(|s| s == criterion))
        })
    });
    if attached {
        sources.insert(USER_EVIDENCE_OWNER.to_owned());
    }
    sources
}

/// The first verifier whose latest result on `criterion` is `fail` with no
/// later `not_reproduced` cross-check on it.
fn unanswered_fail<'a>(session: &'a Session, criterion: &str) -> Option<&'a PartId> {
    session
        .latest_with(criterion, Outcome::Fail)
        .into_iter()
        .find(|fail| {
            !session.cross_checks.iter().any(|c| {
                c.round > fail.round
                    && c.outcome == CrossOutcome::NotReproduced
                    && matches!(&c.subject, Subject::Result { part, criterion: on }
                        if *part == fail.part && on == criterion)
            })
        })
        .map(|r| &r.part)
}

/// Whether `check` reproduced a `fail` result on `criterion` (as that
/// result stood before the check's round).
fn reproduces_fail(session: &Session, criterion: &str, check: &CrossCheck) -> bool {
    check.outcome == CrossOutcome::Reproduced
        && matches!(&check.subject, Subject::Result { part, criterion: on }
            if on == criterion
                && session
                    .result_as_of(part, criterion, check.round.saturating_sub(1))
                    .is_some_and(|r| r.outcome == Outcome::Fail))
}

/// The first `reproduced` cross-check on a `fail` result on `criterion`:
/// a failure another verifier confirmed.
fn reproduced_fail<'a>(session: &'a Session, criterion: &str) -> Option<(&'a PartId, &'a PartId)> {
    session.cross_checks.iter().find_map(|c| match &c.subject {
        Subject::Result { part, .. } if reproduces_fail(session, criterion, c) => {
            Some((&c.by, part))
        }
        _ => None,
    })
}

/// Whether `key` (evidence for `criterion`) backs it failing: cited by a
/// `fail` result on it, or by a `reproduced` cross-check that reproduced a
/// `fail` result on it or a defect; a reproduced pass backs no failure. A
/// command or file whose file was not stored backs nothing.
fn backs_failure(session: &Session, criterion: &str, key: &EvidenceKey) -> bool {
    let counts = session
        .evidence_item(key)
        .is_some_and(|e| e.file_path().is_none() || e.stored.is_some());
    counts
        && (session.results.iter().any(|r| {
            r.criterion == criterion && r.outcome == Outcome::Fail && r.evidence.contains(key)
        }) || session.cross_checks.iter().any(|c| {
            c.evidence.contains(key)
                && (reproduces_fail(session, criterion, c)
                    || (c.outcome == CrossOutcome::Reproduced
                        && matches!(c.subject, Subject::Defect { .. })))
        }))
}

/// Why `ruling` on criterion `criterion` in `round` breaks the evidence
/// standard, as one problem line (`AC-2: verified needs a matched re-run or
/// two independent passes`).
pub fn check_ruling(
    session: &Session,
    _round: RoundNo,
    criterion: &str,
    ruling: &Ruling,
) -> Result<(), String> {
    for key in &ruling.evidence {
        if session.evidence_item(key).is_none() {
            return Err(format!(
                "{criterion}: cited evidence {:?} does not exist (cite keys as <verifier>/r<N>/<id> or user/<n>)",
                key.as_str()
            ));
        }
    }
    if matches!(ruling.kind, RulingKind::Verified | RulingKind::Failed)
        && let Some(key) = ruling
            .evidence
            .iter()
            .find(|k| !belongs(session, criterion, k))
    {
        return Err(format!(
            "{criterion}: cited evidence {key} is not evidence for {criterion} (cite what a result or a cross-check on {criterion} cites, or media whose shows names it)"
        ));
    }
    let cited: Vec<&Evidence> = ruling
        .evidence
        .iter()
        .filter_map(|k| session.evidence_item(k))
        .collect();
    match ruling.kind {
        RulingKind::Verified => {
            if !cited.is_empty() && !cited.iter().any(|e| is_command_or_file(e)) {
                return Err(format!(
                    "{criterion}: an observation alone never makes a criterion verified; cite a command or a file"
                ));
            }
            if let Some(part) = unanswered_fail(session, criterion) {
                return Err(format!(
                    "{criterion}: verified despite {part}'s fail, which no not_reproduced cross-check has answered"
                ));
            }
            if let Some((by, part)) = reproduced_fail(session, criterion) {
                return Err(format!(
                    "{criterion}: verified despite {part}'s fail, which {by} reproduced"
                ));
            }
            let matched = cited.iter().any(|e| is_matched_command(e));
            if !matched && independent_passes(session, criterion, &ruling.evidence).len() < 2 {
                let unstored: Vec<&str> = cited
                    .iter()
                    .filter(|e| is_command_or_file(e) && e.stored.is_none())
                    .map(|e| e.key.as_str())
                    .collect();
                return Err(if unstored.is_empty() {
                    format!(
                        "{criterion}: verified needs a matched re-run or two independent passes"
                    )
                } else {
                    format!(
                        "{criterion}: verified needs a matched re-run or two independent passes; {} was not stored (gone before ingest) and counts for nothing",
                        unstored.join(", ")
                    )
                });
            }
        }
        RulingKind::Failed => {
            if !ruling
                .evidence
                .iter()
                .any(|k| backs_failure(session, criterion, k))
            {
                return Err(format!(
                    "{criterion}: failed needs a cited fail result or reproduced evidence"
                ));
            }
        }
        RulingKind::Inconclusive | RulingKind::NotApplicable => {
            if ruling.rationale.trim().is_empty() {
                return Err(format!(
                    "{criterion}: {} needs a rationale",
                    ruling.kind.name()
                ));
            }
        }
    }
    Ok(())
}

/// Why `verdict` does not follow from `rulings` (this round's, every
/// criterion) and `defects` (as this round's defect ops leave them).
///
/// A `should` criterion ruled `not_applicable` holds nothing back. A
/// `must` one was not verified: it refuses `pass`, and allows
/// `pass_with_issues` only with it listed in `blocking` (L1-6).
/// `pass_with_issues` lists in `blocking` everything that holds the change
/// back: each `should` criterion not verified, each `must` one ruled
/// `not_applicable`, each live defect.
pub fn check_verdict(
    session: &Session,
    rulings: &BTreeMap<String, Ruling>,
    defects: &[Defect],
    verdict: &Verdict,
) -> Result<(), String> {
    let kind = |id: &str| rulings.get(id).map(|r| r.kind);
    let of = |priority: Priority, want: RulingKind| -> Vec<&str> {
        session
            .criteria
            .iter()
            .filter(|c| c.priority == priority && kind(&c.id) == Some(want))
            .map(|c| c.id.as_str())
            .collect()
    };
    let unverified = |priority: Priority| -> Vec<&str> {
        session
            .criteria
            .iter()
            .filter(|c| {
                c.priority == priority
                    && !matches!(
                        kind(&c.id),
                        Some(RulingKind::Verified | RulingKind::NotApplicable)
                    )
            })
            .map(|c| c.id.as_str())
            .collect()
    };
    let live: Vec<&Defect> = defects.iter().filter(|d| d.status.is_live()).collect();
    let serious: Vec<&str> = live
        .iter()
        .filter(|d| d.severity <= Severity::High)
        .map(|d| d.id.as_str())
        .collect();
    for id in &verdict.blocking {
        if session.criterion(id).is_none() && !live.iter().any(|d| d.id == *id) {
            return Err(format!(
                "verdict.blocking: {id:?} is neither a criterion nor a live defect"
            ));
        }
    }
    let failed_must = of(Priority::Must, RulingKind::Failed);
    let must_open = unverified(Priority::Must);
    let must_not_applicable = of(Priority::Must, RulingKind::NotApplicable);
    match verdict.decision {
        Decision::Pass | Decision::PassWithIssues if !must_open.is_empty() => Err(format!(
            "verdict: {} needs every must criterion verified; {} {} not",
            verdict.decision.name(),
            must_open.join(", "),
            if must_open.len() == 1 { "is" } else { "are" }
        )),
        Decision::Pass if !must_not_applicable.is_empty() => Err(format!(
            "verdict: pass needs every must criterion verified; {} (must) {} ruled not_applicable; use pass_with_issues with {} in blocking",
            must_not_applicable.join(", "),
            if must_not_applicable.len() == 1 {
                "was"
            } else {
                "were"
            },
            if must_not_applicable.len() == 1 {
                "it"
            } else {
                "them"
            }
        )),
        Decision::Pass if !serious.is_empty() => Err(format!(
            "verdict: pass with live critical or high defect {}; use pass_with_issues or fail",
            serious.join(", ")
        )),
        Decision::Pass if !verdict.blocking.is_empty() => {
            Err("verdict: pass takes no blocking; use pass_with_issues".to_owned())
        }
        Decision::PassWithIssues => {
            let issues: Vec<&str> = must_not_applicable
                .iter()
                .copied()
                .chain(unverified(Priority::Should))
                .chain(live.iter().map(|d| d.id.as_str()))
                .collect();
            if issues.is_empty() {
                return Err(
                    "verdict: nothing holds the change back (every should criterion verified, no live defect); use pass"
                        .to_owned(),
                );
            }
            if verdict.blocking.is_empty() {
                return Err(format!(
                    "verdict: pass_with_issues needs blocking to list what holds it back: {}",
                    issues.join(", ")
                ));
            }
            if let Some(id) = verdict
                .blocking
                .iter()
                .find(|id| !issues.contains(&id.as_str()))
            {
                return Err(format!(
                    "verdict.blocking: {id} holds nothing back (only a should criterion not verified, a must criterion ruled not_applicable or a live defect does)"
                ));
            }
            let missing: Vec<&str> = issues
                .iter()
                .copied()
                .filter(|id| !verdict.blocking.iter().any(|b| b == id))
                .collect();
            if missing.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "verdict.blocking: missing {}; pass_with_issues lists everything that holds the change back",
                    missing.join(", ")
                ))
            }
        }
        Decision::Fail if failed_must.is_empty() => {
            Err("verdict: fail needs a must criterion ruled failed".to_owned())
        }
        Decision::Fail if verdict.blocking.is_empty() => Err(format!(
            "verdict: fail needs blocking to list what fails: {}",
            failed_must.join(", ")
        )),
        Decision::Inconclusive if !failed_must.is_empty() => Err(format!(
            "verdict: {} (must) failed; the decision is fail",
            failed_must.join(", ")
        )),
        Decision::Inconclusive if of(Priority::Must, RulingKind::Inconclusive).is_empty() => {
            Err("verdict: inconclusive needs a must criterion ruled inconclusive".to_owned())
        }
        _ => Ok(()),
    }
}

/// The assignments round `round` gets when the judge gave none: every
/// `fail` and every `pass` resting on a single verifier, each given to a
/// verifier other than its reporter, round-robin.
pub fn default_assignments(session: &Session, round: RoundNo) -> BTreeMap<PartId, Vec<Subject>> {
    let before = round.saturating_sub(1);
    let verifiers: Vec<&PartId> = session.verifier_ids().collect();
    let mut subjects: Vec<(PartId, Subject)> = Vec::new();
    for criterion in &session.criteria {
        let latest: Vec<_> = verifiers
            .iter()
            .filter_map(|part| session.result_as_of(part, &criterion.id, before))
            .collect();
        let subject = |part: &PartId| Subject::Result {
            part: part.clone(),
            criterion: criterion.id.clone(),
        };
        for r in latest.iter().filter(|r| r.outcome == Outcome::Fail) {
            subjects.push((r.part.clone(), subject(&r.part)));
        }
        let passes: Vec<_> = latest
            .iter()
            .filter(|r| r.outcome == Outcome::Pass)
            .collect();
        if let [single] = passes.as_slice() {
            subjects.push((single.part.clone(), subject(&single.part)));
        }
    }
    let mut out: BTreeMap<PartId, Vec<Subject>> = BTreeMap::new();
    if verifiers.len() < 2 {
        return out;
    }
    let mut next = 0;
    for (reporter, subject) in subjects {
        if *verifiers[next % verifiers.len()] == reporter {
            next += 1;
        }
        out.entry(verifiers[next % verifiers.len()].clone())
            .or_default()
            .push(subject);
        next += 1;
    }
    out
}

/// Whether round 1 may close early with `rulings`: every criterion
/// verified by two or more verifiers with re-runs that matched.
pub fn early_close_allowed(session: &Session, rulings: &BTreeMap<String, Ruling>) -> bool {
    !session.criteria.is_empty()
        && session.criteria.iter().all(|c| {
            rulings.get(&c.id).map(|r| r.kind) == Some(RulingKind::Verified)
                && session
                    .latest_with(&c.id, Outcome::Pass)
                    .into_iter()
                    .filter(|r| {
                        r.evidence
                            .iter()
                            .filter_map(|k| session.evidence_item(k))
                            .any(is_matched_command)
                    })
                    .count()
                    >= 2
        })
}
