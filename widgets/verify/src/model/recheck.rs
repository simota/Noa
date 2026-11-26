//! The previous verification carried into a new run. Only the criteria
//! become current state; the old outcomes remain comparison data and
//! never take part in this run's evidence standard.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::types::{
    Criterion, MAX_CRITERIA, MAX_STATEMENT, Millis, Outcome, Rebuild, RulingKind, Session,
    SessionId, SpecFile, Target,
};

/// A self-contained comparison baseline, so deleting or moving the old
/// session cannot change what the new run says it was checked against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recheck {
    pub parent: SessionId,
    pub captured_at: Millis,
    pub target: Target,
    pub rebuild: Rebuild,
    /// The spec the inherited sources refer to; repeated rechecks keep
    /// that version even when the newly captured spec has changed.
    pub spec: Option<SpecFile>,
    pub criteria: Vec<Criterion>,
    /// Keep removed ids reserved when the person adds another criterion.
    pub removed_criteria: Vec<Criterion>,
    pub user_criteria: Vec<String>,
    pub rulings: BTreeMap<String, RulingKind>,
    /// Criteria with at least one pass/fail result in the previous run.
    pub tested: BTreeSet<String>,
}

impl Recheck {
    pub fn from_session(session: &Session) -> Option<Self> {
        if !session.criteria_accepted || session.criteria.is_empty() {
            return None;
        }
        Some(Self {
            parent: session.id.clone(),
            captured_at: session.manifest.captured_at,
            target: session.target.clone(),
            rebuild: session.rebuild.clone(),
            spec: session
                .recheck
                .as_ref()
                .map_or_else(|| session.spec.clone(), |previous| previous.spec.clone()),
            criteria: session.criteria.clone(),
            removed_criteria: session.removed_criteria.clone(),
            user_criteria: session.user_criteria.clone(),
            rulings: session
                .criteria
                .iter()
                .filter_map(|c| session.ruling(&c.id).map(|r| (c.id.clone(), r.kind)))
                .collect(),
            tested: session
                .criteria
                .iter()
                .filter(|c| tested(session, &c.id))
                .map(|c| c.id.clone())
                .collect(),
        })
    }

    /// Reused ids may have gaps or be out of order after edits. They must
    /// remain unique, including ids removed before the previous run.
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.parent.as_str().trim().is_empty() {
            return Err("再検証の元のセッションがありません".into());
        }
        if !(1..=MAX_CRITERIA).contains(&self.criteria.len()) {
            return Err(format!("引き継ぐ受け入れ基準は1〜{MAX_CRITERIA}件です"));
        }
        let mut ids = BTreeSet::new();
        for c in self.criteria.iter().chain(&self.removed_criteria) {
            let number = c.id.strip_prefix("AC-").and_then(|n| n.parse::<u32>().ok());
            if !number.is_some_and(|n| n > 0 && n < u32::MAX) || !ids.insert(&c.id) {
                return Err(format!(
                    "引き継ぐ基準のIDが不正または重複しています: {}",
                    c.id
                ));
            }
            if c.statement.trim().is_empty()
                || c.statement.chars().count() > MAX_STATEMENT
                || c.how.trim().is_empty()
            {
                return Err(format!("引き継ぐ基準の内容を確認してください: {}", c.id));
            }
        }
        Ok(())
    }
}

fn tested(session: &Session, id: &str) -> bool {
    session
        .results
        .iter()
        .any(|r| r.criterion == id && matches!(r.outcome, Outcome::Pass | Outcome::Fail))
}

/// A change of statement, method, priority or source makes two outcomes
/// unsuitable for a direct before/after comparison. The edited marker
/// itself does not change what was tested.
fn same_criterion(before: &Criterion, after: &Criterion) -> bool {
    before.id == after.id
        && before.statement == after.statement
        && before.how == after.how
        && before.priority == after.priority
        && before.source == after.source
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Resolved,
    Continuing,
    NewFailure,
    NowTested,
    NowVerified,
    Unconfirmed,
    Unchanged,
    ChangedCriterion,
    RemovedCriterion,
    AddedCriterion,
}

impl Change {
    pub fn label(self) -> &'static str {
        match self {
            Self::Resolved => "解消",
            Self::Continuing => "継続",
            Self::NewFailure => "新規失敗",
            Self::NowTested => "検証可能化",
            Self::NowVerified => "検証済へ",
            Self::Unconfirmed => "今回未確認",
            Self::Unchanged => "変化なし",
            Self::ChangedCriterion => "基準変更・比較対象外",
            Self::RemovedCriterion => "基準削除・比較対象外",
            Self::AddedCriterion => "基準追加",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub id: String,
    pub statement: String,
    pub before: Option<RulingKind>,
    pub after: Option<RulingKind>,
    pub changes: Vec<Change>,
}

/// Compare only the latest rulings of this fresh run. A missing,
/// inconclusive or not-applicable ruling never resolves an old failure.
/// A newly runnable check can also expose a new failure, so both labels
/// are kept rather than hiding one behind the other.
pub fn compare(session: &Session) -> Vec<Comparison> {
    let Some(previous) = &session.recheck else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for before in &previous.criteria {
        let after = session.criterion(&before.id);
        let old = previous.rulings.get(&before.id).copied();
        let new = after.and_then(|c| session.ruling(&c.id)).map(|r| r.kind);
        let mut changes = match after {
            None => vec![Change::RemovedCriterion],
            Some(after) if !same_criterion(before, after) => vec![Change::ChangedCriterion],
            Some(_) => {
                let change = match (old, new) {
                    (Some(RulingKind::Failed), Some(RulingKind::Verified)) => Change::Resolved,
                    (Some(RulingKind::Failed), Some(RulingKind::Failed)) => Change::Continuing,
                    (_, Some(RulingKind::Failed)) => Change::NewFailure,
                    (Some(RulingKind::Verified), Some(RulingKind::Verified)) => Change::Unchanged,
                    (_, Some(RulingKind::Verified)) => Change::NowVerified,
                    _ => Change::Unconfirmed,
                };
                vec![change]
            }
        };
        if after.is_some_and(|after| same_criterion(before, after))
            && !previous.tested.contains(&before.id)
            && tested(session, &before.id)
        {
            if changes == [Change::Unchanged] || changes == [Change::NowVerified] {
                changes.clear();
            }
            changes.push(Change::NowTested);
        }
        rows.push(Comparison {
            id: before.id.clone(),
            statement: after.unwrap_or(before).statement.clone(),
            before: old,
            after: new,
            changes,
        });
    }
    for criterion in &session.criteria {
        if previous.criteria.iter().any(|c| c.id == criterion.id) {
            continue;
        }
        let after = session.ruling(&criterion.id).map(|r| r.kind);
        let mut changes = vec![Change::AddedCriterion];
        match after {
            Some(RulingKind::Failed) => changes.push(Change::NewFailure),
            Some(RulingKind::Verified) => {}
            _ => changes.push(Change::Unconfirmed),
        }
        rows.push(Comparison {
            id: criterion.id.clone(),
            statement: criterion.statement.clone(),
            before: None,
            after,
            changes,
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::{CriterionResult, CriterionSource, Priority, Ruling};
    use crate::testkit::{self, p};

    fn fixture() -> Session {
        let mut session = testkit::Rig::new(2);
        testkit::accept_default_criteria(&mut session);
        session.engine.session().clone()
    }

    fn rule(session: &mut Session, id: &str, kind: RulingKind) {
        session.rulings.insert(
            id.into(),
            vec![Ruling {
                round: 1,
                kind,
                rationale: String::new(),
                evidence: Vec::new(),
            }],
        );
    }

    #[test]
    fn the_comparison_never_carries_a_previous_pass_into_the_new_run() {
        let mut session = fixture();
        rule(&mut session, "AC-1", RulingKind::Failed);
        rule(&mut session, "AC-2", RulingKind::Verified);
        rule(&mut session, "AC-3", RulingKind::Failed);
        session.recheck = Recheck::from_session(&session);
        session.rulings.clear();
        assert!(
            compare(&session)
                .iter()
                .all(|r| r.changes == [Change::Unconfirmed])
        );
        rule(&mut session, "AC-1", RulingKind::Verified);
        rule(&mut session, "AC-2", RulingKind::Failed);
        rule(&mut session, "AC-3", RulingKind::Failed);
        let rows = compare(&session);
        assert_eq!(rows[0].changes, [Change::Resolved]);
        assert_eq!(rows[1].changes, [Change::NewFailure]);
        assert_eq!(rows[2].changes, [Change::Continuing]);
        rule(&mut session, "AC-1", RulingKind::NotApplicable);
        assert_eq!(compare(&session)[0].changes, [Change::Unconfirmed]);
    }

    #[test]
    fn newly_runnable_failure_is_not_presented_as_only_an_improvement() {
        let mut session = fixture();
        session.recheck = Recheck::from_session(&session);
        session.results.push(CriterionResult {
            round: 1,
            part: p("codex"),
            criterion: "AC-1".into(),
            outcome: Outcome::Fail,
            note: "now runnable".into(),
            evidence: Vec::new(),
        });
        rule(&mut session, "AC-1", RulingKind::Failed);
        assert_eq!(
            compare(&session)[0].changes,
            [Change::NewFailure, Change::NowTested]
        );
    }

    #[test]
    fn changed_and_removed_criteria_are_not_counted_as_resolved() {
        let mut session = fixture();
        rule(&mut session, "AC-1", RulingKind::Failed);
        rule(&mut session, "AC-2", RulingKind::Failed);
        session.recheck = Recheck::from_session(&session);
        session.criteria[0].how = "a different check".into();
        session.criteria.remove(1);
        rule(&mut session, "AC-1", RulingKind::Verified);
        let rows = compare(&session);
        assert_eq!(rows[0].changes, [Change::ChangedCriterion]);
        assert_eq!(rows[1].changes, [Change::RemovedCriterion]);
    }

    #[test]
    fn an_inconclusive_ruling_becoming_verified_is_not_unchanged() {
        let mut session = fixture();
        rule(&mut session, "AC-1", RulingKind::Inconclusive);
        session.recheck = Recheck::from_session(&session);
        rule(&mut session, "AC-1", RulingKind::Verified);
        assert_eq!(compare(&session)[0].changes, [Change::NowVerified]);
        let mut added = session.criteria[0].clone();
        added.id = "AC-4".into();
        session.criteria.push(added);
        rule(&mut session, "AC-4", RulingKind::Failed);
        assert_eq!(
            compare(&session).last().unwrap().changes,
            [Change::AddedCriterion, Change::NewFailure]
        );
    }

    #[test]
    fn the_baseline_keeps_structured_criteria_and_reserves_removed_ids() {
        let mut session = fixture();
        session.criteria[0].how = "a custom procedure".into();
        session.criteria[0].priority = Priority::Should;
        session.criteria[0].source = CriterionSource::Spec { line: 8 };
        session.criteria[0].edited = true;
        let removed = session.criteria.remove(1);
        session.removed_criteria.push(removed);
        session.criteria.reverse();
        let baseline = Recheck::from_session(&session).unwrap();
        assert_eq!(baseline.criteria, session.criteria);
        assert_eq!(baseline.removed_criteria, session.removed_criteria);
        assert!(baseline.check().is_ok());
        let encoded = serde_json::to_string(&baseline).unwrap();
        assert_eq!(serde_json::from_str::<Recheck>(&encoded).unwrap(), baseline);
        let mut duplicate = baseline.clone();
        duplicate.criteria.push(duplicate.criteria[0].clone());
        assert!(duplicate.check().is_err());
    }

    #[test]
    fn old_session_json_without_a_recheck_keeps_the_original_behavior() {
        let original = fixture();
        let mut value = serde_json::to_value(&original).unwrap();
        value.as_object_mut().unwrap().remove("recheck");
        let read: Session = serde_json::from_value(value).unwrap();
        assert_eq!(read, original);
        assert!(read.recheck.is_none());
        assert!(compare(&read).is_empty());
    }

    #[test]
    fn repeated_rechecks_keep_the_original_spec_source_and_only_the_direct_parent() {
        let mut session = fixture();
        session.spec = Some(SpecFile {
            path: "spec.md".into(),
            bytes: 10,
            lines: 2,
            digest: 11,
        });
        session.recheck = Recheck::from_session(&session);
        session.id = SessionId::new("ver-second");
        session.spec.as_mut().unwrap().digest = 22;
        let next = Recheck::from_session(&session).unwrap();
        assert_eq!(next.parent.as_str(), "ver-second");
        assert_eq!(next.spec.unwrap().digest, 11);
    }

    #[test]
    fn the_report_keeps_parent_and_dirty_target_versions_with_uncertain_results() {
        let mut session = fixture();
        rule(&mut session, "AC-1", RulingKind::Failed);
        session.recheck = Recheck::from_session(&session);
        session.id = SessionId::new("ver-again");
        session.rebuild.untracked.push(("probe.rs".into(), 42));
        session.rulings.clear();
        let report = super::super::export::report_markdown(&session, None);
        assert!(report.contains("## 前回との比較"));
        assert!(report.contains("前回の検証: `ver-1`"));
        assert!(report.contains("| 不合格 | 未判定 | 今回未確認 |"));
        let versions: Vec<_> = report
            .lines()
            .filter(|line| line.contains("対象版（HEAD / 構成識別子）"))
            .map(|line| line.split(": ").nth(1).unwrap())
            .collect();
        assert_eq!(versions.len(), 2);
        assert_ne!(versions[0], versions[1]);
    }
}
