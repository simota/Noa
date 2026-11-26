//! Assets and their lifecycle, verification state, improvements (L2-3).
//! Pure: nothing here reads or writes a file.
//!
//! An asset's lifecycle is its last decision's `to` (Decision 3); its
//! verification state is derived separately from observations (Decision 9).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hash;
use crate::scan::Evidence;
use crate::time::Millis;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    Checker,
    Runbook,
    Rule,
}

impl AssetKind {
    pub fn label(self) -> &'static str {
        match self {
            AssetKind::Checker => "チェッカー",
            AssetKind::Runbook => "手順書",
            AssetKind::Rule => "ルール",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lifecycle {
    Candidate,
    Trial,
    Evaluating,
    Adopted,
    Reviewing,
    Retired,
}

impl Lifecycle {
    pub const ALL: [Lifecycle; 6] = [
        Lifecycle::Candidate,
        Lifecycle::Trial,
        Lifecycle::Evaluating,
        Lifecycle::Adopted,
        Lifecycle::Reviewing,
        Lifecycle::Retired,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Lifecycle::Candidate => "候補",
            Lifecycle::Trial => "試作",
            Lifecycle::Evaluating => "評価中",
            Lifecycle::Adopted => "採用",
            Lifecycle::Reviewing => "見直し",
            Lifecycle::Retired => "廃止",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lifecycle::Candidate => "candidate",
            Lifecycle::Trial => "trial",
            Lifecycle::Evaluating => "evaluating",
            Lifecycle::Adopted => "adopted",
            Lifecycle::Reviewing => "reviewing",
            Lifecycle::Retired => "retired",
        }
    }

    /// The stages a decision may move this one to.
    pub fn next(self) -> &'static [Lifecycle] {
        use Lifecycle::*;
        match self {
            Candidate => &[Trial, Retired],
            Trial => &[Evaluating, Retired],
            Evaluating => &[Adopted, Trial, Retired],
            Adopted => &[Reviewing, Retired],
            Reviewing => &[Adopted, Evaluating, Retired],
            Retired => &[],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceKind {
    Verify,
    Review,
    Task,
    Path,
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub kind: EvidenceKind,
    pub r#ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub to: Lifecycle,
    pub at: String,
    pub reason: String,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub id: String,
    pub kind: AssetKind,
    pub title: String,
    pub path: String,
    pub decisions: Vec<Decision>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionError {
    InvalidTransition { from: Lifecycle, to: Lifecycle },
    NoEvidence,
    EmptyReason,
}

impl std::fmt::Display for DecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecisionError::InvalidTransition { from, to } => {
                write!(f, "{}から{}へは移せません", from.label(), to.label())
            }
            DecisionError::NoEvidence => write!(f, "採用には根拠が1件以上必要です"),
            DecisionError::EmptyReason => write!(f, "理由を書いてください"),
        }
    }
}

/// Whether `d` may follow stage `from`.
fn check(from: Lifecycle, d: &Decision) -> Result<(), DecisionError> {
    if !from.next().contains(&d.to) {
        return Err(DecisionError::InvalidTransition { from, to: d.to });
    }
    if d.reason.trim().is_empty() {
        return Err(DecisionError::EmptyReason);
    }
    if d.to == Lifecycle::Adopted && d.evidence.is_empty() {
        return Err(DecisionError::NoEvidence);
    }
    Ok(())
}

impl Asset {
    /// The last decision's `to`, else `Candidate`.
    pub fn lifecycle(&self) -> Lifecycle {
        self.decisions.last().map_or(Lifecycle::Candidate, |d| d.to)
    }

    /// Appends `d` when it is a valid next step.
    pub fn decide(&mut self, d: Decision) -> Result<(), DecisionError> {
        check(self.lifecycle(), &d)?;
        self.decisions.push(d);
        Ok(())
    }
}

/// Replays a hand-edited log: the first decision that could not have been
/// recorded, with its index (shown as 記録に矛盾).
pub fn check_history(asset: &Asset) -> Result<(), (usize, DecisionError)> {
    let mut at = Lifecycle::Candidate;
    for (i, d) in asset.decisions.iter().enumerate() {
        check(at, d).map_err(|e| (i, e))?;
        at = d.to;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyState {
    Verified,
    Failed,
    NotRun,
    Skipped,
    NotRunnable,
    Unknown,
    Stale,
}

impl VerifyState {
    pub fn label(self) -> &'static str {
        match self {
            VerifyState::Verified => "確認済み",
            VerifyState::Failed => "失敗",
            VerifyState::NotRun => "未実施",
            VerifyState::Skipped => "省略",
            VerifyState::NotRunnable => "実行不能",
            VerifyState::Unknown => "不明",
            VerifyState::Stale => "現版未確認",
        }
    }
}

/// What an observation is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Subject {
    Asset(String),
    Improvement(String),
}

/// What a Verify session's log says (L2-4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionFact {
    Missing,
    Unreadable(String),
    Registered,
    Running,
    Finished {
        decision: Option<String>,
        commit: Option<String>,
    },
}

/// One standing observation: a link that was not removed, or a skip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Verify {
        subject: Subject,
        session: String,
        path: PathBuf,
        at: Millis,
    },
    Review {
        subject: Subject,
        session: String,
        path: PathBuf,
        at: Millis,
    },
    Task {
        subject: Subject,
        id: String,
        path: PathBuf,
        at: Millis,
    },
    Skipped {
        subject: Subject,
        reason: String,
        commit: Option<String>,
        at: Millis,
    },
}

impl Observation {
    pub fn subject(&self) -> &Subject {
        match self {
            Observation::Verify { subject, .. }
            | Observation::Review { subject, .. }
            | Observation::Task { subject, .. }
            | Observation::Skipped { subject, .. } => subject,
        }
    }

    pub fn at(&self) -> Millis {
        match self {
            Observation::Verify { at, .. }
            | Observation::Review { at, .. }
            | Observation::Task { at, .. }
            | Observation::Skipped { at, .. } => *at,
        }
    }
}

/// The verification state of a subject from its observations, each Verify
/// link with its session's fact, at commit `current` (L2-3). A registered
/// or running Verify is never read as passed.
pub fn verification(
    obs: &[(Observation, Option<SessionFact>)],
    current: Option<&str>,
) -> VerifyState {
    let newest = obs
        .iter()
        .filter(|(o, f)| {
            matches!(o, Observation::Skipped { .. })
                || (matches!(o, Observation::Verify { .. })
                    && matches!(f, Some(SessionFact::Finished { .. })))
        })
        .max_by_key(|(o, _)| o.at());
    match newest {
        Some((Observation::Skipped { .. }, _)) => return VerifyState::Skipped,
        Some((_, Some(SessionFact::Finished { decision, commit }))) => {
            let Some(commit) = commit else {
                return VerifyState::Stale;
            };
            let Some(current) = current else {
                return VerifyState::Unknown;
            };
            if commit != current {
                return VerifyState::Stale;
            }
            return match decision.as_deref() {
                Some("pass") => VerifyState::Verified,
                // Q3: issues remain, so not 確認済み.
                Some("fail") | Some("pass_with_issues") => VerifyState::Failed,
                Some("inconclusive") | None => VerifyState::NotRunnable,
                Some(_) => VerifyState::Unknown,
            };
        }
        _ => {}
    }
    let unplaced = obs.iter().any(|(o, f)| {
        matches!(o, Observation::Verify { .. })
            && matches!(
                f,
                None | Some(SessionFact::Missing) | Some(SessionFact::Unreadable(_))
            )
    });
    if unplaced {
        VerifyState::Unknown
    } else {
        VerifyState::NotRun
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Improvement {
    pub id: String,
    pub rule: String,
    pub widget: String,
    pub target: String,
    pub evidence: Vec<Evidence>,
    pub first_seen: Millis,
    pub last_seen: Millis,
    pub resolved: Option<Millis>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImprovementStatus {
    Open,
    HandedOff(String),
    Resolved,
}

impl ImprovementStatus {
    pub fn label(&self) -> String {
        match self {
            ImprovementStatus::Open => "未着手".to_owned(),
            ImprovementStatus::HandedOff(id) => format!("引き渡し済み（{id}）"),
            ImprovementStatus::Resolved => "解消".to_owned(),
        }
    }
}

/// 解消 once the scan no longer finds it; else 引き渡し済み with the newest
/// task linked to it; else 未着手.
pub fn improvement_status(imp: &Improvement, obs: &[Observation]) -> ImprovementStatus {
    if imp.resolved.is_some() {
        return ImprovementStatus::Resolved;
    }
    let subject = Subject::Improvement(imp.id.clone());
    obs.iter()
        .filter_map(|o| match o {
            Observation::Task {
                subject: s, id, at, ..
            } if *s == subject => Some((*at, id)),
            _ => None,
        })
        .max_by_key(|(at, _)| *at)
        .map_or(ImprovementStatus::Open, |(_, id)| {
            ImprovementStatus::HandedOff(id.clone())
        })
}

/// `imp-` and 16 hex digits of FNV-1a 64 over
/// `<repo identity>\n<rule>\n<widget>/<target>` (I3).
pub fn candidate_id(identity: &str, rule: &str, widget: &str, target: &str) -> String {
    format!(
        "imp-{}",
        hash::hex(format!("{identity}\n{rule}\n{widget}/{target}").as_bytes())
    )
}

/// The Japanese label of a rule id (I3).
pub fn rule_label(rule: &str) -> &'static str {
    match rule {
        "missing-registration" => "登録漏れ",
        "incomplete-registration" => "登録が不完全",
        "name-mismatch" => "名前の不一致",
        "repo-arg-mismatch" => "--repo の不一致",
        "no-intent" => "registry に意図がない",
        "stale-intent" => "registry の古い意図",
        "ship-missing" => "配布漏れ",
        "ship-unintended" => "意図しない配布",
        "os-difference-unexplained" => "OS 差の理由がない",
        "os-difference" => "OS 差（理由あり）",
        _ => "不明な規則",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(to: Lifecycle, reason: &str, evidence: usize) -> Decision {
        Decision {
            to,
            at: "2026-11-26".into(),
            reason: reason.into(),
            evidence: (0..evidence)
                .map(|i| EvidenceRef {
                    kind: EvidenceKind::Verify,
                    r#ref: format!("ver-{i}"),
                })
                .collect(),
        }
    }

    fn asset(decisions: Vec<Decision>) -> Asset {
        Asset {
            id: "a".into(),
            kind: AssetKind::Checker,
            title: "A".into(),
            path: "scripts/a.py".into(),
            decisions,
        }
    }

    #[test]
    fn lifecycle_is_the_last_decision() {
        assert_eq!(asset(vec![]).lifecycle(), Lifecycle::Candidate);
        let a = asset(vec![
            d(Lifecycle::Trial, "r", 0),
            d(Lifecycle::Evaluating, "r", 0),
        ]);
        assert_eq!(a.lifecycle(), Lifecycle::Evaluating);
        assert_eq!(check_history(&a), Ok(()));
    }

    #[test]
    fn adopting_without_evidence_is_refused() {
        let mut a = asset(vec![
            d(Lifecycle::Trial, "r", 0),
            d(Lifecycle::Evaluating, "r", 0),
        ]);
        assert_eq!(
            a.decide(d(Lifecycle::Adopted, "効いた", 0)),
            Err(DecisionError::NoEvidence)
        );
        assert_eq!(a.decisions.len(), 2);
        assert_eq!(a.decide(d(Lifecycle::Adopted, "効いた", 1)), Ok(()));
        assert_eq!(a.lifecycle(), Lifecycle::Adopted);
    }

    #[test]
    fn transitions_follow_the_table() {
        let mut a = asset(vec![]);
        assert_eq!(
            a.decide(d(Lifecycle::Adopted, "r", 1)),
            Err(DecisionError::InvalidTransition {
                from: Lifecycle::Candidate,
                to: Lifecycle::Adopted
            })
        );
        assert_eq!(
            a.decide(d(Lifecycle::Trial, "  ", 0)),
            Err(DecisionError::EmptyReason)
        );
        a.decide(d(Lifecycle::Retired, "不要", 0)).unwrap();
        for to in Lifecycle::ALL {
            assert!(a.decide(d(to, "r", 1)).is_err(), "{to:?} after retired");
        }
        let expected: [(Lifecycle, &[Lifecycle]); 6] = [
            (
                Lifecycle::Candidate,
                &[Lifecycle::Trial, Lifecycle::Retired],
            ),
            (
                Lifecycle::Trial,
                &[Lifecycle::Evaluating, Lifecycle::Retired],
            ),
            (
                Lifecycle::Evaluating,
                &[Lifecycle::Adopted, Lifecycle::Trial, Lifecycle::Retired],
            ),
            (
                Lifecycle::Adopted,
                &[Lifecycle::Reviewing, Lifecycle::Retired],
            ),
            (
                Lifecycle::Reviewing,
                &[
                    Lifecycle::Adopted,
                    Lifecycle::Evaluating,
                    Lifecycle::Retired,
                ],
            ),
            (Lifecycle::Retired, &[]),
        ];
        for (from, to) in expected {
            assert_eq!(from.next(), to);
        }
    }

    #[test]
    fn a_contradictory_history_names_its_first_bad_entry() {
        let a = asset(vec![
            d(Lifecycle::Trial, "r", 0),
            d(Lifecycle::Adopted, "r", 1),
        ]);
        assert_eq!(
            check_history(&a),
            Err((
                1,
                DecisionError::InvalidTransition {
                    from: Lifecycle::Trial,
                    to: Lifecycle::Adopted
                }
            ))
        );
    }

    fn subject() -> Subject {
        Subject::Asset("a".into())
    }

    fn ver(at: Millis, fact: Option<SessionFact>) -> (Observation, Option<SessionFact>) {
        (
            Observation::Verify {
                subject: subject(),
                session: format!("ver-{at}"),
                path: PathBuf::from("/v"),
                at,
            },
            fact,
        )
    }

    fn finished(decision: Option<&str>, commit: Option<&str>) -> Option<SessionFact> {
        Some(SessionFact::Finished {
            decision: decision.map(str::to_owned),
            commit: commit.map(str::to_owned),
        })
    }

    #[test]
    fn a_registered_or_running_verify_is_not_run_never_verified() {
        let head = Some("abc");
        assert_eq!(verification(&[], head), VerifyState::NotRun);
        assert_eq!(
            verification(&[ver(1, Some(SessionFact::Registered))], head),
            VerifyState::NotRun
        );
        assert_eq!(
            verification(&[ver(1, Some(SessionFact::Running))], head),
            VerifyState::NotRun
        );
        assert_eq!(
            verification(&[ver(1, Some(SessionFact::Missing))], head),
            VerifyState::Unknown
        );
        assert_eq!(
            verification(&[ver(1, Some(SessionFact::Unreadable("x".into())))], head),
            VerifyState::Unknown
        );
    }

    #[test]
    fn a_finished_pass_counts_only_on_the_current_commit() {
        let pass = |c| ver(1, finished(Some("pass"), c));
        assert_eq!(
            verification(&[pass(Some("abc"))], Some("abc")),
            VerifyState::Verified
        );
        assert_eq!(
            verification(&[pass(Some("old"))], Some("abc")),
            VerifyState::Stale
        );
        assert_eq!(verification(&[pass(None)], Some("abc")), VerifyState::Stale);
        assert_eq!(
            verification(&[pass(Some("abc"))], None),
            VerifyState::Unknown
        );
        let with = |d| verification(&[ver(1, finished(d, Some("abc")))], Some("abc"));
        assert_eq!(with(Some("fail")), VerifyState::Failed);
        assert_eq!(with(Some("pass_with_issues")), VerifyState::Failed);
        assert_eq!(with(Some("inconclusive")), VerifyState::NotRunnable);
        assert_eq!(with(None), VerifyState::NotRunnable);
        assert_eq!(with(Some("odd")), VerifyState::Unknown);
    }

    #[test]
    fn the_newest_finished_or_skip_decides() {
        let skip = |at| {
            (
                Observation::Skipped {
                    subject: subject(),
                    reason: "r".into(),
                    commit: None,
                    at,
                },
                None,
            )
        };
        let obs = [ver(1, finished(Some("pass"), Some("abc"))), skip(2)];
        assert_eq!(verification(&obs, Some("abc")), VerifyState::Skipped);
        let obs = [skip(1), ver(2, finished(Some("fail"), Some("abc")))];
        assert_eq!(verification(&obs, Some("abc")), VerifyState::Failed);
        // A running session after a pass does not hide the pass.
        let obs = [
            ver(1, finished(Some("pass"), Some("abc"))),
            ver(2, Some(SessionFact::Running)),
        ];
        assert_eq!(verification(&obs, Some("abc")), VerifyState::Verified);
    }

    #[test]
    fn improvement_status_is_derived() {
        let mut imp = Improvement {
            id: "imp-1".into(),
            rule: "missing-registration".into(),
            widget: "g".into(),
            target: "launch".into(),
            evidence: vec![],
            first_seen: 1,
            last_seen: 1,
            resolved: None,
        };
        assert_eq!(improvement_status(&imp, &[]), ImprovementStatus::Open);
        let task = Observation::Task {
            subject: Subject::Improvement("imp-1".into()),
            id: "t-5".into(),
            path: PathBuf::from("/r/.noa/tasks/t-5.md"),
            at: 5,
        };
        assert_eq!(
            improvement_status(&imp, std::slice::from_ref(&task)),
            ImprovementStatus::HandedOff("t-5".into())
        );
        imp.resolved = Some(9);
        assert_eq!(
            improvement_status(&imp, &[task]),
            ImprovementStatus::Resolved
        );
    }

    #[test]
    fn candidate_ids_are_stable_and_distinct() {
        let a = candidate_id("/r/.git", "missing-registration", "g", "launch");
        assert_eq!(a.len(), 4 + 16);
        assert!(a.starts_with("imp-"));
        assert_eq!(
            a,
            format!(
                "imp-{:016x}",
                hash::fnv1a64(b"/r/.git\nmissing-registration\ng/launch")
            )
        );
        assert_ne!(
            a,
            candidate_id("/s/.git", "missing-registration", "g", "launch")
        );
        assert_ne!(
            a,
            candidate_id("/r/.git", "missing-registration", "g", "verify")
        );
    }
}
