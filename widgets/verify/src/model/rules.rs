//! The evidence standard the widget holds the judge to (L1-6, Judge
//! ruling): what each ruling needs, the verdict's consistency with the
//! rulings, the default cross-check assignments (L1-4 item 4) and when
//! round 1 may close early (L1-4 item 5). Pure.
//!
//! The standard reads a criterion's whole history, gathered once into a
//! [`History`]: every result on the criterion, every cross-check on one of
//! those results with the one result it is about, every cross-check on a
//! defect, and what each evidence item is to the criterion. L1-6's rule 1
//! is [`Check::about`], set in [`History::of`]; rules 2–6 are `History`'s
//! methods, in L1-6's order and words; what `verified` and `failed` need
//! (`History::refuse_verified`, `History::refuse_failed`, called by
//! [`check_ruling`]) only combines them. A rule changes in L1-6, in the
//! ruling brief's [`EVIDENCE_STANDARD`](crate::model::brief::EVIDENCE_STANDARD)
//! and here, together.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::types::{
    CriterionResult, CrossCheck, CrossOutcome, Decision, Defect, Evidence, EvidenceBody,
    EvidenceKey, Media, Outcome, PartId, Priority, Rerun, RoundNo, Ruling, RulingKind, Session,
    Severity, Subject, USER_EVIDENCE_OWNER, Verdict,
};

/// One criterion's history (L1-6): every result on it from every verifier
/// and round, every cross-check on one of those results with the result it
/// is about, every cross-check on a defect, and, through
/// [`History::standing`], what each evidence item is to the criterion.
pub struct History<'a> {
    session: &'a Session,
    criterion: &'a str,
    /// Every result on the criterion, from every verifier and round, in the
    /// order recorded.
    pub results: Vec<&'a CriterionResult>,
    /// Every cross-check whose subject is a result on the criterion, with
    /// the result it is about (rule 1), in the order recorded.
    pub checks: Vec<Check<'a>>,
    /// Every cross-check on a defect: one backs a failure through the
    /// evidence it cites (rule 6), whatever became of the defect since.
    pub defect_checks: Vec<&'a CrossCheck>,
    /// What the results and the checks above cite.
    cited: BTreeSet<&'a EvidenceKey>,
}

/// A cross-check on a result, and the one result it is about.
#[derive(Debug, Clone, Copy)]
pub struct Check<'a> {
    pub check: &'a CrossCheck,
    /// Rule 1: its subject's latest result on the criterion as of the round
    /// before the check, the result the check's brief named
    /// (`- codex/AC-1: pass (R2)`). `None` only for a subject with no result
    /// then, which validation does not let through.
    pub about: Option<&'a CriterionResult>,
}

/// What an evidence item is to one criterion (rule 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// No item has the key.
    Unknown,
    /// Not evidence for the criterion: nothing on it cites the item, or the
    /// item is media whose `shows` leaves the criterion out.
    Elsewhere,
    /// Evidence for the criterion whose file was missing at ingest: it
    /// counts for nothing.
    Unstored,
    /// Evidence for the criterion that counts: an observation (text, with
    /// no file to store).
    Observation,
    /// Evidence for the criterion that counts: a command or a file the
    /// widget stored.
    Stored,
}

impl Standing {
    /// Whether the item counts for the criterion (rule 2).
    pub fn counts(self) -> bool {
        matches!(self, Standing::Observation | Standing::Stored)
    }
}

/// A `fail` that no cross-check answered (rule 4), with why the
/// `not_reproduced` checks on its subject did not.
pub struct OpenFail<'a> {
    pub fail: &'a CriterionResult,
    /// `not_reproduced` checks on its subject, from its round on, that are
    /// about another result of its verifier.
    pub about_other: Vec<Check<'a>>,
    /// Cited by `not_reproduced` checks about it, but not stored.
    pub unstored: Vec<&'a EvidenceKey>,
    /// Text-only observations cannot discharge a recorded failure.
    pub observations: Vec<&'a EvidenceKey>,
    /// Cited by those checks, but media whose `shows` leaves the criterion
    /// out.
    pub elsewhere: Vec<&'a EvidenceKey>,
}

impl<'a> History<'a> {
    pub fn of(session: &'a Session, criterion: &'a str) -> Self {
        let results: Vec<&CriterionResult> = session
            .results
            .iter()
            .filter(|r| r.criterion == criterion)
            .collect();
        let checks: Vec<Check> = session
            .cross_checks
            .iter()
            .filter_map(|check| match &check.subject {
                Subject::Result {
                    part,
                    criterion: on,
                } if on == criterion => Some(Check {
                    check,
                    about: session.result_as_of(part, criterion, check.round.saturating_sub(1)),
                }),
                _ => None,
            })
            .collect();
        let defect_checks = session
            .cross_checks
            .iter()
            .filter(|c| matches!(c.subject, Subject::Defect { .. }))
            .collect();
        let cited = results
            .iter()
            .flat_map(|r| &r.evidence)
            .chain(checks.iter().flat_map(|c| &c.check.evidence))
            .collect();
        History {
            session,
            criterion,
            results,
            checks,
            defect_checks,
            cited,
        }
    }

    /// Rule 2: an item is evidence for the criterion when a result on it or
    /// a check about one cites it, or it is media whose `shows` names it
    /// (media with `shows` is evidence only for the criteria it names,
    /// whoever cites it); it counts when its file was stored (an
    /// observation has none).
    pub fn standing(&self, key: &EvidenceKey) -> Standing {
        let Some(item) = self.session.evidence_item(key) else {
            return Standing::Unknown;
        };
        let evidence_for = match &item.body {
            EvidenceBody::File { shows, .. } if !shows.is_empty() => {
                shows.iter().any(|s| s == self.criterion)
            }
            _ => self.cited.contains(key),
        };
        if !evidence_for {
            Standing::Elsewhere
        } else if matches!(item.body, EvidenceBody::Observation { .. }) {
            Standing::Observation
        } else if item.stored.is_some() {
            Standing::Stored
        } else {
            Standing::Unstored
        }
    }

    /// Every verifier's `pass` on the criterion, of any round.
    fn passes(&self) -> impl Iterator<Item = &'a CriterionResult> + '_ {
        self.results
            .iter()
            .copied()
            .filter(|r| r.outcome == Outcome::Pass && self.session.is_verifier(&r.part))
    }

    /// The passes a ruling citing `cited` would rest on but for a contest
    /// (rule 3): each one of whose commands or files `cited` holds and that
    /// counts for the criterion.
    fn cited_passes<'c>(
        &'c self,
        cited: &'c [EvidenceKey],
    ) -> impl Iterator<Item = &'a CriterionResult> + 'c {
        self.passes().filter(|r| {
            r.evidence
                .iter()
                .any(|k| cited.contains(k) && self.standing(k) == Standing::Stored)
        })
    }

    /// Rule 3: the passes that count for a ruling citing `cited`: each
    /// verifier's `pass` of any round one of whose commands or files
    /// `cited` holds and that counts for the criterion, unless it is
    /// contested ([`History::contesting`]).
    pub fn counted_passes(&self, cited: &[EvidenceKey]) -> Vec<&'a CriterionResult> {
        self.cited_passes(cited)
            .filter(|r| self.contesting(r).is_empty())
            .collect()
    }

    /// Rule 3: the cross-checks that contest `pass` and still stand: each
    /// a `not_reproduced` about that very result (rule 1) citing evidence
    /// that counts (rule 2), with no `pass` on the criterion of a later
    /// round than the check that has a command or file that counts for the
    /// criterion (media whose `shows` leaves it out has none) and is not
    /// itself contested. Its checker is another verifier, as a verifier is
    /// never assigned its own result. A contested pass counts for no
    /// ruling; a `disputed` check contests nothing.
    pub fn contesting(&self, pass: &CriterionResult) -> Vec<&'a CrossCheck> {
        self.checks
            .iter()
            .filter(|c| {
                self.not_reproduces(c, pass)
                    && !self.passes().any(|later| {
                        later.round > c.check.round
                            && later
                                .evidence
                                .iter()
                                .any(|k| self.standing(k) == Standing::Stored)
                            && self.contesting(later).is_empty()
                    })
            })
            .map(|c| c.check)
            .collect()
    }

    /// Rule 3: the independent sources a ruling citing `cited` rests on:
    /// each verifier with a counted pass, however many, and the person
    /// when `cited` holds media they attached that is stored evidence for
    /// the criterion.
    pub fn sources(&self, cited: &[EvidenceKey]) -> BTreeSet<&'a str> {
        let mut sources: BTreeSet<&str> = self
            .counted_passes(cited)
            .into_iter()
            .map(|r| r.part.as_str())
            .collect();
        if cited
            .iter()
            .any(|k| k.is_user() && self.standing(k) == Standing::Stored)
        {
            sources.insert(USER_EVIDENCE_OWNER);
        }
        sources
    }

    /// Whether `cited` holds a command of a counted pass (rule 3) whose
    /// re-run matched: enough alone for `verified`.
    pub fn matched_rerun_of_a_pass(&self, cited: &[EvidenceKey]) -> bool {
        !self.matched_passes(cited).is_empty()
    }

    /// The verifiers with a pass that counts for a ruling citing `cited`
    /// (rule 3) and a command of that pass, cited and stored, whose re-run
    /// matched: the verifiers a round-1 early close counts (L1-4 item 5).
    /// A verifier the ruling does not cite counts for nothing, however its
    /// re-runs went.
    pub fn matched_passes(&self, cited: &[EvidenceKey]) -> BTreeSet<&'a PartId> {
        self.counted_passes(cited)
            .into_iter()
            .filter(|pass| {
                pass.evidence.iter().any(|k| {
                    cited.contains(k)
                        && self.standing(k) == Standing::Stored
                        && self.rerun_matched(k)
                })
            })
            .map(|pass| &pass.part)
            .collect()
    }

    fn rerun_matched(&self, key: &EvidenceKey) -> bool {
        self.session.evidence_item(key).is_some_and(|e| {
            matches!(e.body, EvidenceBody::Command { .. })
                && matches!(e.rerun, Some(Rerun::Matched { .. }))
        })
    }

    /// Whether `check` is a `not_reproduced` about that very `result`
    /// (rule 1). A fail needs a stored command or file for the criterion
    /// to answer it (rule 4); text can still contest a pass (rule 3).
    fn not_reproduces(&self, check: &Check, result: &CriterionResult) -> bool {
        check.check.outcome == CrossOutcome::NotReproduced
            && check.about.is_some_and(|r| std::ptr::eq(r, result))
            && check.check.evidence.iter().any(|k| {
                let standing = self.standing(k);
                if result.outcome == Outcome::Fail {
                    standing == Standing::Stored
                } else {
                    standing.counts()
                }
            })
    }

    /// Rule 4: every `fail` of any round that no `not_reproduced` about it
    /// answered with stored command or file evidence, oldest first.
    /// Nothing else answers one: not a text-only observation, not a later
    /// result of its verifier, not a `disputed` or `reproduced` check, not
    /// a `not_reproduced` about another result of its verifier.
    pub fn open_fails(&self) -> Vec<OpenFail<'a>> {
        let fails = self.results.iter().copied().filter(|r| {
            r.outcome == Outcome::Fail && !self.checks.iter().any(|c| self.not_reproduces(c, r))
        });
        fails
            .map(|fail| {
                let mut open = OpenFail {
                    fail,
                    about_other: Vec::new(),
                    unstored: Vec::new(),
                    observations: Vec::new(),
                    elsewhere: Vec::new(),
                };
                for c in &self.checks {
                    let on_its_subject = matches!(&c.check.subject,
                        Subject::Result { part, .. } if *part == fail.part);
                    if c.check.outcome != CrossOutcome::NotReproduced
                        || !on_its_subject
                        || c.check.round < fail.round
                    {
                        continue;
                    }
                    if !c.about.is_some_and(|r| std::ptr::eq(r, fail)) {
                        open.about_other.push(*c);
                        continue;
                    }
                    for key in &c.check.evidence {
                        let list = match self.standing(key) {
                            Standing::Unstored => &mut open.unstored,
                            Standing::Observation => &mut open.observations,
                            Standing::Elsewhere => &mut open.elsewhere,
                            _ => continue,
                        };
                        if !list.contains(&key) {
                            list.push(key);
                        }
                    }
                }
                open
            })
            .collect()
    }

    /// Rule 5: each `fail` that a `reproduced` cross-check about it
    /// reproduced, with that check: confirmed, whatever comes later.
    pub fn confirmed_fails(&self) -> Vec<(&'a CriterionResult, &'a CrossCheck)> {
        self.checks
            .iter()
            .filter(|c| c.check.outcome == CrossOutcome::Reproduced)
            .filter_map(|c| {
                c.about
                    .filter(|r| r.outcome == Outcome::Fail)
                    .map(|r| (r, c.check))
            })
            .collect()
    }

    /// Rule 6: whether `key` backs the criterion failing: it counts for the
    /// criterion, and a `fail` result on it cites it, or a `reproduced`
    /// check about a `fail` result on it, or a `reproduced` check on a
    /// defect. A check about a `pass` backs no failure, whatever its
    /// outcome.
    pub fn backs_failure(&self, key: &EvidenceKey) -> bool {
        let reproduced = CrossOutcome::Reproduced;
        self.standing(key).counts()
            && (self
                .results
                .iter()
                .any(|r| r.outcome == Outcome::Fail && r.evidence.contains(key))
                || self.checks.iter().any(|c| {
                    c.check.outcome == reproduced
                        && c.about.is_some_and(|r| r.outcome == Outcome::Fail)
                        && c.check.evidence.contains(key)
                })
                || self
                    .defect_checks
                    .iter()
                    .any(|c| c.outcome == reproduced && c.evidence.contains(key)))
    }

    /// Why a `verified` ruling citing `cited` breaks the standard: it
    /// needs a cited command or file, no open fail (rule 4), no confirmed
    /// fail (rule 5), and a matched re-run of a counted pass's command or
    /// two sources (rule 3).
    fn refuse_verified(&self, cited: &[EvidenceKey]) -> Option<String> {
        let criterion = self.criterion;
        let items: Vec<&Evidence> = cited
            .iter()
            .filter_map(|k| self.session.evidence_item(k))
            .collect();
        if !items.is_empty()
            && items
                .iter()
                .all(|e| matches!(e.body, EvidenceBody::Observation { .. }))
        {
            return Some(format!(
                "{criterion}: an observation alone never makes a criterion verified; cite a command or a file"
            ));
        }
        if let Some(open) = self.open_fails().first() {
            return Some(open.refusal(criterion));
        }
        if let Some((fail, check)) = self.confirmed_fails().first() {
            return Some(format!(
                "{criterion}: verified despite {}'s R{} fail, which {} reproduced in R{}",
                fail.part, fail.round, check.by, check.round
            ));
        }
        if self.matched_rerun_of_a_pass(cited) || self.sources(cited).len() >= 2 {
            return None;
        }
        let mut why =
            format!("{criterion}: verified needs a matched re-run or two independent passes");
        let unstored: Vec<&EvidenceKey> = cited
            .iter()
            .filter(|k| self.standing(k) == Standing::Unstored)
            .collect();
        if !unstored.is_empty() {
            why.push_str(&format!(
                "; {} was not stored (gone before ingest) and counts for nothing",
                keys(&unstored)
            ));
        }
        let off_pass: Vec<&EvidenceKey> = cited
            .iter()
            .filter(|k| {
                self.standing(k) == Standing::Stored
                    && self.rerun_matched(k)
                    && !self
                        .results
                        .iter()
                        .any(|r| r.outcome == Outcome::Pass && r.evidence.contains(k))
            })
            .collect();
        if !off_pass.is_empty() {
            why.push_str(&format!(
                "; {} re-ran matched, but no pass on {criterion} cites {}",
                keys(&off_pass),
                if off_pass.len() == 1 { "it" } else { "them" }
            ));
        }
        for pass in self.cited_passes(cited) {
            let against = self.contesting(pass);
            if against.is_empty() {
                continue;
            }
            let by: Vec<String> = against
                .iter()
                .map(|c| format!("{}'s R{} not_reproduced", c.by, c.round))
                .collect();
            why.push_str(&format!(
                "; {}'s R{} pass is contested by {} and counts for nothing until a later pass that is not contested",
                pass.part,
                pass.round,
                by.join(", ")
            ));
        }
        Some(why)
    }

    /// Why a `failed` ruling citing `cited` breaks the standard: it needs
    /// one item that backs the criterion failing (rule 6).
    fn refuse_failed(&self, cited: &[EvidenceKey]) -> Option<String> {
        (!cited.iter().any(|k| self.backs_failure(k))).then(|| {
            format!(
                "{}: failed needs a cited fail result or reproduced evidence",
                self.criterion
            )
        })
    }
}

impl OpenFail<'_> {
    /// The refusal of a `verified` ruling while this fail is open: its
    /// verifier and round, then why each `not_reproduced` on its subject
    /// did not answer it.
    fn refusal(&self, criterion: &str) -> String {
        let mut why = format!(
            "{criterion}: verified despite {}'s R{} fail, which no not_reproduced cross-check has answered",
            self.fail.part, self.fail.round
        );
        for c in &self.about_other {
            if let Some(r) = c.about {
                why.push_str(&format!(
                    "; {}'s R{} not_reproduced is about {}'s R{} {}",
                    c.check.by,
                    c.check.round,
                    r.part,
                    r.round,
                    r.outcome.name()
                ));
            }
        }
        if !self.unstored.is_empty() {
            why.push_str(&format!(
                "; {} was not stored (gone before ingest) and counts for nothing",
                keys(&self.unstored)
            ));
        }
        if !self.elsewhere.is_empty() {
            why.push_str(&format!(
                "; {} is not evidence for {criterion} (its shows leaves {criterion} out)",
                keys(&self.elsewhere)
            ));
        }
        if !self.observations.is_empty() {
            why.push_str(&format!(
                "; {} is text-only observation; answering a fail needs a stored command or file for {criterion}",
                keys(&self.observations)
            ));
        }
        why
    }
}

fn keys(list: &[&EvidenceKey]) -> String {
    list.iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(", ")
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
    let history = History::of(session, criterion);
    if let Some(key) = ruling
        .evidence
        .iter()
        .find(|k| history.standing(k) == Standing::Unknown)
    {
        return Err(format!(
            "{criterion}: cited evidence {:?} does not exist (cite keys as <verifier>/r<N>/<id> or user/<n>)",
            key.as_str()
        ));
    }
    let on_evidence = matches!(ruling.kind, RulingKind::Verified | RulingKind::Failed);
    if on_evidence
        && let Some(key) = ruling
            .evidence
            .iter()
            .find(|k| history.standing(k) == Standing::Elsewhere)
    {
        return Err(format!(
            "{criterion}: cited evidence {key} is not evidence for {criterion} (cite what a result or a cross-check on {criterion} cites, or media whose shows names it)"
        ));
    }
    let refusal = match ruling.kind {
        RulingKind::Verified => history.refuse_verified(&ruling.evidence),
        RulingKind::Failed => history.refuse_failed(&ruling.evidence),
        RulingKind::Inconclusive | RulingKind::NotApplicable => ruling
            .rationale
            .trim()
            .is_empty()
            .then(|| format!("{criterion}: {} needs a rationale", ruling.kind.name())),
    };
    if let Some(why) = refusal {
        return Err(why);
    }
    // A video cannot be watched by the judge: a ruling resting on videos
    // alone rests on their captions and marks, and says which (L1-5a).
    let stored: Vec<&Evidence> = ruling
        .evidence
        .iter()
        .filter(|k| history.standing(k) == Standing::Stored)
        .filter_map(|k| session.evidence_item(k))
        .collect();
    if on_evidence
        && ruling.rationale.trim().is_empty()
        && !stored.is_empty()
        && stored
            .iter()
            .all(|e| e.media().is_some_and(Media::is_video))
    {
        let keys: Vec<&str> = stored.iter().map(|e| e.key.as_str()).collect();
        return Err(format!(
            "{criterion}: {} rests on video evidence alone ({}); its rationale must name the caption and marks it relied on",
            ruling.kind.name(),
            keys.join(", ")
        ));
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
/// verified by two or more verifiers with re-runs that matched, counting
/// on each criterion only the verifiers its ruling rests on
/// ([`History::matched_passes`]).
pub fn early_close_allowed(session: &Session, rulings: &BTreeMap<String, Ruling>) -> bool {
    !session.criteria.is_empty()
        && session.criteria.iter().all(|c| {
            rulings.get(&c.id).is_some_and(|r| {
                r.kind == RulingKind::Verified
                    && History::of(session, &c.id)
                        .matched_passes(&r.evidence)
                        .len()
                        >= 2
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::{Attachment, Confidence, DefectStatus, Stored};
    use crate::testkit::{self, Rig, p};

    /// One step of a criterion's history. Evidence ids are a reply's own
    /// (`e1`): an `e…` id is a command, `o…` an observation, `f…` an image,
    /// each stored unless [`Step::Lost`].
    enum Step {
        /// A result: round, verifier, criterion, outcome, evidence ids.
        Res(
            RoundNo,
            &'static str,
            &'static str,
            Outcome,
            &'static [&'static str],
        ),
        /// A cross-check: round, by, subject (`codex/AC-1` or `D1`),
        /// outcome, evidence ids.
        Chk(
            RoundNo,
            &'static str,
            &'static str,
            CrossOutcome,
            &'static [&'static str],
        ),
        /// The command's re-run matched.
        Matched(&'static str),
        /// The item's file was gone before ingest.
        Lost(&'static str),
        /// The media item's `shows`.
        Shows(&'static str, &'static [&'static str]),
        /// The person attached `user/<n>` showing these criteria.
        Attach(u32, &'static [&'static str]),
        /// The judge opened this defect.
        Opened(&'static str),
        /// The judge merged the first defect into the second.
        Merged(&'static str, &'static str),
    }

    /// The default panel (claude, codex, claude-2) and criteria (AC-1 …
    /// AC-3), round 1 started, nothing reported yet.
    fn base() -> Session {
        let mut rig = Rig::new(4);
        testkit::accept_default_criteria(&mut rig);
        testkit::start_round_one(&mut rig);
        rig.engine.session().clone()
    }

    fn item<'s>(session: &'s mut Session, key: &str) -> &'s mut Evidence {
        session
            .evidence_item_mut(&testkit::key(key))
            .unwrap_or_else(|| panic!("no evidence {key}"))
    }

    /// The keys of `part`'s `ids` in `round`, each item made on first use.
    fn cite(
        session: &mut Session,
        round: RoundNo,
        part: &str,
        ids: &[&str],
        exit: i32,
    ) -> Vec<EvidenceKey> {
        let mut keys = Vec::new();
        for id in ids {
            let key = EvidenceKey::reported(&p(part), round, id);
            if session.evidence_item(&key).is_none() {
                let (body, path) = match &id[..1] {
                    "e" => {
                        let log = format!("logs/{id}.log");
                        let body = EvidenceBody::Command {
                            cmd: format!("cargo test {id}"),
                            cwd: ".".into(),
                            exit,
                            log: log.clone(),
                            rerun: false,
                        };
                        (body, Some(log))
                    }
                    "o" => (
                        EvidenceBody::Observation {
                            text: "seen".into(),
                        },
                        None,
                    ),
                    "f" => {
                        let path = format!("shots/{id}.png");
                        let body = EvidenceBody::File {
                            path: path.clone(),
                            caption: Some("what it shows".into()),
                            media: Some(Media::Png),
                            marks: Vec::new(),
                            shows: Vec::new(),
                        };
                        (body, Some(path))
                    }
                    other => panic!("no evidence kind {other:?}"),
                };
                session.evidence.push(Evidence {
                    key: key.clone(),
                    body,
                    stored: path.map(|path| testkit::stored_for(round, &p(part), &path)),
                    rerun: None,
                    attached: None,
                });
            }
            keys.push(key);
        }
        keys
    }

    fn defect(id: &str) -> Defect {
        Defect {
            id: id.into(),
            claim: format!("claim {id}"),
            repro: "1. run".into(),
            location: None,
            in_target: true,
            in_diff: None,
            severity: Severity::High,
            category: "falsify".into(),
            confidence: Confidence::High,
            status: DefectStatus::Open,
            merged_into: None,
            rationale: "seen".into(),
            fix: None,
            sources: Vec::new(),
            opened_in: 1,
            changed_rev: 0,
            history: Vec::new(),
        }
    }

    fn history(steps: &[Step]) -> Session {
        let mut session = base();
        for step in steps {
            match *step {
                Step::Res(round, part, criterion, outcome, ids) => {
                    let exit = if outcome == Outcome::Fail { 1 } else { 0 };
                    let evidence = cite(&mut session, round, part, ids, exit);
                    session.results.push(CriterionResult {
                        round,
                        part: p(part),
                        criterion: criterion.into(),
                        outcome,
                        note: "noted".into(),
                        evidence,
                    });
                }
                Step::Chk(round, by, subject, outcome, ids) => {
                    let evidence = cite(&mut session, round, by, ids, 0);
                    session.cross_checks.push(CrossCheck {
                        round,
                        by: p(by),
                        subject: Subject::parse(subject).unwrap(),
                        outcome,
                        note: "noted".into(),
                        evidence,
                    });
                }
                Step::Matched(key) => {
                    let e = item(&mut session, key);
                    let EvidenceBody::Command { exit, rerun, .. } = &mut e.body else {
                        panic!("{key} is not a command");
                    };
                    *rerun = true;
                    e.rerun = Some(Rerun::Matched {
                        exit: *exit,
                        log: None,
                    });
                }
                Step::Lost(key) => item(&mut session, key).stored = None,
                Step::Shows(key, criteria) => {
                    let EvidenceBody::File { shows, .. } = &mut item(&mut session, key).body else {
                        panic!("{key} is not a file");
                    };
                    *shows = criteria.iter().map(|c| c.to_string()).collect();
                }
                Step::Attach(n, criteria) => session.evidence.push(Evidence {
                    key: EvidenceKey::user(n),
                    body: EvidenceBody::File {
                        path: format!("shot-{n}.png"),
                        caption: Some("the person's shot".into()),
                        media: Some(Media::Png),
                        marks: Vec::new(),
                        shows: criteria.iter().map(|c| c.to_string()).collect(),
                    },
                    stored: Some(Stored {
                        path: format!("evidence/user/{n}.png").into(),
                        bytes: 100,
                        digest: 1,
                    }),
                    rerun: None,
                    attached: Some(Attachment { round: 1, at: 1 }),
                }),
                Step::Opened(id) => session.defects.push(defect(id)),
                Step::Merged(from, into) => {
                    let d = session.defect_mut(from).unwrap();
                    d.status = DefectStatus::Merged;
                    d.merged_into = Some(into.into());
                }
            }
        }
        session
    }

    #[test]
    fn ac13_each_multi_round_history_gets_the_decision_the_standard_gives() {
        use CrossOutcome::{Disputed, NotReproduced, Reproduced};
        use Outcome::{Blocked, Fail, Pass};
        use RulingKind::{Failed, Verified};
        use Step::*;
        const OPEN: &str = "AC-1: verified despite codex's R1 fail, which no not_reproduced cross-check has answered";
        const TWO: &str = "AC-1: verified needs a matched re-run or two independent passes";
        const CONTESTED: &str = "claude's R1 pass is contested by claude-2's R2 not_reproduced and counts for nothing until a later pass that is not contested";
        const NOT_BACKED: &str = "AC-1: failed needs a cited fail result or reproduced evidence";
        let not_for = |key: &str| {
            format!(
                "AC-1: cited evidence {key} is not evidence for AC-1 (cite what a result or a cross-check on AC-1 cites, or media whose shows names it)"
            )
        };
        // claude passes AC-1 on a command whose re-run matched: on its own
        // enough for verified, once nothing stands against AC-1.
        let claude_matched = || {
            [
                Res(1, "claude", "AC-1", Pass, &["e1"]),
                Matched("claude/r1/e1"),
            ]
        };
        /// A history's name and steps, a ruling on AC-1 (its kind and the
        /// evidence it cites) and what the standard says to it.
        type Case = (
            &'static str,
            Vec<Step>,
            RulingKind,
            &'static [&'static str],
            Result<(), String>,
        );
        let cases: Vec<Case> = vec![
            // When a pass counts (rule 3).
            (
                "one pass whose command re-ran matched",
                claude_matched().into(),
                Verified,
                &["claude/r1/e1"],
                Ok(()),
            ),
            (
                "one pass, not re-run",
                vec![Res(1, "claude", "AC-1", Pass, &["e1"])],
                Verified,
                &["claude/r1/e1"],
                Err(TWO.into()),
            ),
            (
                "two verifiers' passes",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(1, "codex", "AC-1", Pass, &["e1"]),
                ],
                Verified,
                &["claude/r1/e1", "codex/r1/e1"],
                Ok(()),
            ),
            (
                "one verifier's passes in two rounds are one source",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(2, "claude", "AC-1", Pass, &["e1"]),
                ],
                Verified,
                &["claude/r1/e1", "claude/r2/e1"],
                Err(TWO.into()),
            ),
            (
                "two verifiers' passes of different rounds",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(2, "codex", "AC-1", Pass, &["e1"]),
                ],
                Verified,
                &["claude/r1/e1", "codex/r2/e1"],
                Ok(()),
            ),
            (
                "a pass whose log was lost",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(1, "codex", "AC-1", Pass, &["e1"]),
                    Lost("codex/r1/e1"),
                ],
                Verified,
                &["claude/r1/e1", "codex/r1/e1"],
                Err(format!(
                    "{TWO}; codex/r1/e1 was not stored (gone before ingest) and counts for nothing"
                )),
            ),
            (
                "a matched re-run whose log was lost",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Matched("claude/r1/e1"),
                    Lost("claude/r1/e1"),
                ],
                Verified,
                &["claude/r1/e1"],
                Err(format!(
                    "{TWO}; claude/r1/e1 was not stored (gone before ingest) and counts for nothing"
                )),
            ),
            (
                "a pass on another criterion",
                vec![
                    Res(1, "claude", "AC-2", Pass, &["e1"]),
                    Matched("claude/r1/e1"),
                ],
                Verified,
                &["claude/r1/e1"],
                Err(not_for("claude/r1/e1")),
            ),
            (
                "a pass's picture that shows another criterion",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1", "f1"]),
                    Shows("claude/r1/f1", &["AC-2"]),
                    Res(1, "codex", "AC-1", Pass, &["e1"]),
                ],
                Verified,
                &["claude/r1/f1", "codex/r1/e1"],
                Err(not_for("claude/r1/f1")),
            ),
            (
                "the person's picture beside one pass",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Attach(1, &["AC-1"]),
                ],
                Verified,
                &["claude/r1/e1", "user/1"],
                Ok(()),
            ),
            (
                "the person's picture alone",
                vec![Attach(1, &["AC-1"])],
                Verified,
                &["user/1"],
                Err(TWO.into()),
            ),
            (
                "the person's picture of another criterion",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Attach(1, &["AC-2"]),
                ],
                Verified,
                &["claude/r1/e1", "user/1"],
                Err(not_for("user/1")),
            ),
            (
                "an observation alone",
                vec![Res(1, "claude", "AC-1", Pass, &["e1", "o1"])],
                Verified,
                &["claude/r1/o1"],
                Err(
                    "AC-1: an observation alone never makes a criterion verified; cite a command or a file"
                        .into(),
                ),
            ),
            (
                "a reproduced cross-check about a pass is no second pass",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Chk(2, "codex", "claude/AC-1", Reproduced, &["e1"]),
                ],
                Verified,
                &["claude/r1/e1", "codex/r2/e1"],
                Err(TWO.into()),
            ),
            (
                "the matched re-run of a fail's command, the fail answered",
                vec![
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Matched("codex/r1/e1"),
                    Chk(2, "claude-2", "codex/AC-1", NotReproduced, &["e1"]),
                ],
                Verified,
                &["codex/r1/e1"],
                Err(format!(
                    "{TWO}; codex/r1/e1 re-ran matched, but no pass on AC-1 cites it"
                )),
            ),
            // When a pass is contested (rules 1 and 3).
            (
                "a contested pass is no second source",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(1, "codex", "AC-1", Pass, &["e1"]),
                    Chk(2, "claude-2", "claude/AC-1", NotReproduced, &["e1"]),
                ],
                Verified,
                &["claude/r1/e1", "codex/r1/e1"],
                Err(format!("{TWO}; {CONTESTED}")),
            ),
            (
                "a contested pass's matched re-run",
                claude_matched()
                    .into_iter()
                    .chain([Chk(2, "codex", "claude/AC-1", NotReproduced, &["e1"])])
                    .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(format!(
                    "{TWO}; claude's R1 pass is contested by codex's R2 not_reproduced and counts for nothing until a later pass that is not contested"
                )),
            ),
            (
                "a pass contested on an observation",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(1, "codex", "AC-1", Pass, &["e1"]),
                    Chk(2, "claude-2", "claude/AC-1", NotReproduced, &["o1"]),
                ],
                Verified,
                &["claude/r1/e1", "codex/r1/e1"],
                Err(format!("{TWO}; {CONTESTED}")),
            ),
            (
                "a pass contested twice",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(1, "codex", "AC-1", Pass, &["e1"]),
                    Chk(2, "claude-2", "claude/AC-1", NotReproduced, &["e1"]),
                    Chk(3, "codex", "claude/AC-1", NotReproduced, &["e2"]),
                ],
                Verified,
                &["claude/r1/e1", "codex/r1/e1"],
                Err(format!(
                    "{TWO}; claude's R1 pass is contested by claude-2's R2 not_reproduced, codex's R3 not_reproduced and counts for nothing until a later pass that is not contested"
                )),
            ),
            (
                "a pass only disputed is not contested",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(1, "codex", "AC-1", Pass, &["e1"]),
                    Chk(2, "claude-2", "claude/AC-1", Disputed, &[]),
                ],
                Verified,
                &["claude/r1/e1", "codex/r1/e1"],
                Ok(()),
            ),
            (
                "a pass reproduced is not contested",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(1, "codex", "AC-1", Pass, &["e1"]),
                    Chk(2, "claude-2", "claude/AC-1", Reproduced, &["e1"]),
                ],
                Verified,
                &["claude/r1/e1", "codex/r1/e1"],
                Ok(()),
            ),
            (
                "a not_reproduced whose log was lost contests nothing",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(1, "codex", "AC-1", Pass, &["e1"]),
                    Chk(2, "claude-2", "claude/AC-1", NotReproduced, &["e1"]),
                    Lost("claude-2/r2/e1"),
                ],
                Verified,
                &["claude/r1/e1", "codex/r1/e1"],
                Ok(()),
            ),
            (
                "a not_reproduced with a picture of another criterion contests nothing",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(1, "codex", "AC-1", Pass, &["e1"]),
                    Chk(2, "claude-2", "claude/AC-1", NotReproduced, &["f1"]),
                    Shows("claude-2/r2/f1", &["AC-2"]),
                ],
                Verified,
                &["claude/r1/e1", "codex/r1/e1"],
                Ok(()),
            ),
            (
                "a not_reproduced about an earlier fail contests no later pass",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Res(2, "codex", "AC-1", Pass, &["e2"]),
                    Chk(2, "claude-2", "codex/AC-1", NotReproduced, &["e1"]),
                ],
                Verified,
                &["claude/r1/e1", "codex/r2/e2"],
                Ok(()),
            ),
            (
                "a contested pass, then a later pass by another verifier",
                claude_matched()
                    .into_iter()
                    .chain([
                        Chk(2, "codex", "claude/AC-1", NotReproduced, &["e1"]),
                        Res(3, "claude-2", "AC-1", Pass, &["e1"]),
                    ])
                    .collect(),
                Verified,
                &["claude/r1/e1"],
                Ok(()),
            ),
            (
                "a contested pass, then a later pass by its own verifier",
                claude_matched()
                    .into_iter()
                    .chain([
                        Chk(2, "codex", "claude/AC-1", NotReproduced, &["e1"]),
                        Res(3, "claude", "AC-1", Pass, &["e2"]),
                    ])
                    .collect(),
                Verified,
                &["claude/r1/e1"],
                Ok(()),
            ),
            (
                "a contested pass, then a later pass whose only file shows another criterion",
                claude_matched()
                    .into_iter()
                    .chain([
                        Chk(2, "codex", "claude/AC-1", NotReproduced, &["e1"]),
                        Res(3, "claude-2", "AC-1", Pass, &["f1"]),
                        Res(3, "claude-2", "AC-2", Pass, &["f1"]),
                        Shows("claude-2/r3/f1", &["AC-2"]),
                    ])
                    .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(format!(
                    "{TWO}; claude's R1 pass is contested by codex's R2 not_reproduced and counts for nothing until a later pass that is not contested"
                )),
            ),
            (
                "a contested pass, then a later pass whose only log was lost",
                claude_matched()
                    .into_iter()
                    .chain([
                        Chk(2, "codex", "claude/AC-1", NotReproduced, &["e1"]),
                        Res(3, "claude-2", "AC-1", Pass, &["e1"]),
                        Lost("claude-2/r3/e1"),
                    ])
                    .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(format!(
                    "{TWO}; claude's R1 pass is contested by codex's R2 not_reproduced and counts for nothing until a later pass that is not contested"
                )),
            ),
            (
                "a contested pass, then a later pass whose file shows the criterion",
                claude_matched()
                    .into_iter()
                    .chain([
                        Chk(2, "codex", "claude/AC-1", NotReproduced, &["e1"]),
                        Res(3, "claude-2", "AC-1", Pass, &["f1"]),
                        Res(3, "claude-2", "AC-2", Pass, &["f1"]),
                        Shows("claude-2/r3/f1", &["AC-1", "AC-2"]),
                    ])
                    .collect(),
                Verified,
                &["claude/r1/e1"],
                Ok(()),
            ),
            (
                "a contested pass and another verifier's pass of the round of the check",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Chk(2, "codex", "claude/AC-1", NotReproduced, &["e1"]),
                    Res(2, "claude-2", "AC-1", Pass, &["e1"]),
                ],
                Verified,
                &["claude/r1/e1", "claude-2/r2/e1"],
                Err(format!(
                    "{TWO}; claude's R1 pass is contested by codex's R2 not_reproduced and counts for nothing until a later pass that is not contested"
                )),
            ),
            (
                "a contested pass, then a later pass that is itself contested",
                claude_matched()
                    .into_iter()
                    .chain([
                        Chk(2, "codex", "claude/AC-1", NotReproduced, &["e1"]),
                        Res(2, "claude-2", "AC-1", Pass, &["e1"]),
                        Chk(3, "codex", "claude-2/AC-1", NotReproduced, &["e2"]),
                        Res(3, "codex", "AC-1", Pass, &["e3"]),
                        Chk(4, "claude", "codex/AC-1", NotReproduced, &["e2"]),
                    ])
                    .collect(),
                Verified,
                &["claude/r1/e1", "claude-2/r2/e1", "codex/r3/e3"],
                Err(format!(
                    "{TWO}; claude's R1 pass is contested by codex's R2 not_reproduced and counts for nothing until a later pass that is not contested; claude-2's R2 pass is contested by codex's R3 not_reproduced and counts for nothing until a later pass that is not contested; codex's R3 pass is contested by claude's R4 not_reproduced and counts for nothing until a later pass that is not contested"
                )),
            ),
            // When a fail is open or answered (rules 1 and 4).
            (
                "a fail nobody checked",
                [Res(1, "codex", "AC-1", Fail, &["e1"])]
                    .into_iter()
                    .chain(claude_matched())
                    .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(OPEN.into()),
            ),
            (
                "a fail answered by a not_reproduced about it",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Chk(2, "claude-2", "codex/AC-1", NotReproduced, &["e1"]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Ok(()),
            ),
            (
                "a text-only observation cannot answer a recorded fail",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Chk(2, "claude-2", "codex/AC-1", NotReproduced, &["o1"]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(format!("{OPEN}; claude-2/r2/o1 is text-only observation; answering a fail needs a stored command or file for AC-1")),
            ),
            (
                "a fail answered by stored evidence with an additional observation",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Chk(2, "claude-2", "codex/AC-1", NotReproduced, &["o1", "e1"]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Ok(()),
            ),
            (
                "a fail answered by a stored file showing the criterion",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Chk(2, "claude-2", "codex/AC-1", NotReproduced, &["f1"]),
                    Shows("claude-2/r2/f1", &["AC-1"]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Ok(()),
            ),
            (
                "a fail whose answer's log was lost",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Chk(2, "claude-2", "codex/AC-1", NotReproduced, &["e1"]),
                    Lost("claude-2/r2/e1"),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(format!(
                    "{OPEN}; claude-2/r2/e1 was not stored (gone before ingest) and counts for nothing"
                )),
            ),
            (
                "a fail answered with a picture of another criterion",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Chk(2, "claude-2", "codex/AC-1", NotReproduced, &["f1"]),
                    Shows("claude-2/r2/f1", &["AC-2"]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(format!(
                    "{OPEN}; claude-2/r2/f1 is not evidence for AC-1 (its shows leaves AC-1 out)"
                )),
            ),
            (
                "a fail only disputed",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Chk(2, "claude-2", "codex/AC-1", Disputed, &[]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(OPEN.into()),
            ),
            (
                "fail then pass by the same verifier",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Res(2, "codex", "AC-1", Pass, &["e1"]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(OPEN.into()),
            ),
            (
                "fail then blocked by the same verifier",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Res(2, "codex", "AC-1", Blocked, &[]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(OPEN.into()),
            ),
            (
                "fail then pass, checked in the round of the pass (about the fail)",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Res(2, "codex", "AC-1", Pass, &["e1"]),
                    Chk(2, "claude-2", "codex/AC-1", NotReproduced, &["e1"]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Ok(()),
            ),
            (
                "fail then pass, checked a round later (about the pass)",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Res(2, "codex", "AC-1", Pass, &["e1"]),
                    Chk(3, "claude-2", "codex/AC-1", NotReproduced, &["e1"]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(format!(
                    "{OPEN}; claude-2's R3 not_reproduced is about codex's R2 pass"
                )),
            ),
            (
                "two fails by one verifier, only the later one checked",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Res(2, "codex", "AC-1", Fail, &["e1"]),
                    Chk(3, "claude-2", "codex/AC-1", NotReproduced, &["e1"]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(format!(
                    "{OPEN}; claude-2's R3 not_reproduced is about codex's R2 fail"
                )),
            ),
            (
                "pass then fail, checked in the round of the fail (about the pass)",
                claude_matched()
                    .into_iter()
                    .chain([
                        Res(2, "claude", "AC-1", Fail, &["e1"]),
                        Chk(2, "codex", "claude/AC-1", NotReproduced, &["e1"]),
                    ])
                    .collect(),
                Verified,
                &["claude/r1/e1"],
                Err(
                    "AC-1: verified despite claude's R2 fail, which no not_reproduced cross-check has answered; codex's R2 not_reproduced is about claude's R1 pass"
                        .into(),
                ),
            ),
            (
                "pass then fail, checked a round later (about the fail)",
                claude_matched()
                    .into_iter()
                    .chain([
                        Res(2, "claude", "AC-1", Fail, &["e1"]),
                        Chk(3, "codex", "claude/AC-1", NotReproduced, &["e1"]),
                    ])
                    .collect(),
                Verified,
                &["claude/r1/e1"],
                Ok(()),
            ),
            (
                "a reproduced fail stays against the criterion once answered",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Chk(2, "claude-2", "codex/AC-1", Reproduced, &["e1"]),
                    Chk(3, "claude", "codex/AC-1", NotReproduced, &["e1"]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Err("AC-1: verified despite codex's R1 fail, which claude-2 reproduced in R2".into()),
            ),
            (
                "a reproduced check about a later pass confirms no fail",
                [
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Chk(2, "claude-2", "codex/AC-1", NotReproduced, &["e1"]),
                    Res(2, "codex", "AC-1", Pass, &["e2"]),
                    Chk(3, "claude-2", "codex/AC-1", Reproduced, &["e1"]),
                ]
                .into_iter()
                .chain(claude_matched())
                .collect(),
                Verified,
                &["claude/r1/e1"],
                Ok(()),
            ),
            // When a fail backs failed (rule 6).
            (
                "a fail's stored evidence",
                vec![Res(1, "codex", "AC-1", Fail, &["e1"])],
                Failed,
                &["codex/r1/e1"],
                Ok(()),
            ),
            (
                "a fail whose log was lost",
                vec![Res(1, "codex", "AC-1", Fail, &["e1"]), Lost("codex/r1/e1")],
                Failed,
                &["codex/r1/e1"],
                Err(NOT_BACKED.into()),
            ),
            (
                "a pass's evidence",
                vec![Res(1, "claude", "AC-1", Pass, &["e1"])],
                Failed,
                &["claude/r1/e1"],
                Err(NOT_BACKED.into()),
            ),
            (
                "another criterion's fail",
                vec![Res(1, "codex", "AC-2", Fail, &["e1"])],
                Failed,
                &["codex/r1/e1"],
                Err(not_for("codex/r1/e1")),
            ),
            (
                "a reproduced check about a fail",
                vec![
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Chk(2, "claude-2", "codex/AC-1", Reproduced, &["e1"]),
                ],
                Failed,
                &["claude-2/r2/e1"],
                Ok(()),
            ),
            (
                "a reproduced check about the fail, made as its verifier turned to pass",
                vec![
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Res(2, "codex", "AC-1", Pass, &["e2"]),
                    Chk(2, "claude-2", "codex/AC-1", Reproduced, &["e1"]),
                ],
                Failed,
                &["claude-2/r2/e1"],
                Ok(()),
            ),
            (
                "a reproduced check about the later pass",
                vec![
                    Res(1, "codex", "AC-1", Fail, &["e1"]),
                    Res(2, "codex", "AC-1", Pass, &["e2"]),
                    Chk(3, "claude-2", "codex/AC-1", Reproduced, &["e1"]),
                ],
                Failed,
                &["claude-2/r3/e1"],
                Err(NOT_BACKED.into()),
            ),
            (
                "a not_reproduced about a pass",
                vec![
                    Res(1, "claude", "AC-1", Pass, &["e1"]),
                    Chk(2, "codex", "claude/AC-1", NotReproduced, &["e1"]),
                ],
                Failed,
                &["codex/r2/e1"],
                Err(NOT_BACKED.into()),
            ),
            (
                "a reproduced defect, on a picture of the criterion",
                vec![
                    Opened("D1"),
                    Chk(2, "codex", "D1", Reproduced, &["f1"]),
                    Shows("codex/r2/f1", &["AC-1"]),
                ],
                Failed,
                &["codex/r2/f1"],
                Ok(()),
            ),
            (
                "a reproduced defect merged into another since",
                vec![
                    Opened("D1"),
                    Opened("D2"),
                    Chk(2, "codex", "D1", Reproduced, &["f1"]),
                    Shows("codex/r2/f1", &["AC-1"]),
                    Merged("D1", "D2"),
                ],
                Failed,
                &["codex/r2/f1"],
                Ok(()),
            ),
            (
                "a defect not reproduced",
                vec![
                    Opened("D1"),
                    Chk(2, "codex", "D1", NotReproduced, &["f1"]),
                    Shows("codex/r2/f1", &["AC-1"]),
                ],
                Failed,
                &["codex/r2/f1"],
                Err(NOT_BACKED.into()),
            ),
            (
                "a reproduced defect, on a picture of another criterion",
                vec![
                    Opened("D1"),
                    Chk(2, "codex", "D1", Reproduced, &["f1"]),
                    Shows("codex/r2/f1", &["AC-2"]),
                ],
                Failed,
                &["codex/r2/f1"],
                Err(not_for("codex/r2/f1")),
            ),
        ];
        let mut wrong = Vec::new();
        for (name, steps, kind, cited, want) in cases {
            let session = history(&steps);
            let ruling = Ruling {
                round: 4,
                kind,
                rationale: "as cited".into(),
                evidence: cited.iter().map(|k| testkit::key(k)).collect(),
            };
            let got = check_ruling(&session, 4, "AC-1", &ruling);
            if got != want {
                wrong.push(format!("{name}:\n  want {want:?}\n   got {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
    }
}
