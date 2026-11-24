//! Turns what agents write (L1-6) into checked values: the judge's
//! criteria, a verifier's reply (with the facts the runner found at every
//! cited path, so this stays pure), and the judge's rulings. One code
//! fence is tolerated, unknown fields are refused, and every problem is
//! its own line of the correction note.

use std::fmt;

use crate::model::types::{
    CriteriaOutput, CriterionResult, CrossCheck, Evidence, FileFact, JudgeOutput, PartId,
    RawDefect, RoundNo, Session,
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

/// The judge's `c/criteria.json`, checked against the person's lines and
/// the spec (L1-6, Judge criteria).
pub fn parse_criteria(_text: &str, _session: &Session) -> Result<CriteriaOutput, ValidationError> {
    todo!("U2: parse_criteria")
}

/// Every `logs/…` and `file` path a reply cites, so the runner can inspect
/// them before [`parse_reply`]; empty when the text is not a reply.
pub fn cited_paths(_text: &str) -> Vec<String> {
    todo!("U2: cited_paths")
}

/// Verifier `part`'s round-`round` reply, given what was found at every
/// cited path (L1-6, Verifier reply; media per L1-5a).
pub fn parse_reply(
    _text: &str,
    _session: &Session,
    _part: &PartId,
    _round: RoundNo,
    _files: &[FileFact],
) -> Result<Reply, ValidationError> {
    todo!("U2: parse_reply")
}

/// The judge's round-`round` ruling, held to the evidence standard
/// (`model::rules`) and the verdict check.
pub fn parse_judge(
    _text: &str,
    _session: &Session,
    _round: RoundNo,
) -> Result<JudgeOutput, ValidationError> {
    todo!("U2: parse_judge")
}

/// Whether the judge may close the verification in `round` (L1-4 item 5).
pub fn may_close(_session: &Session, _round: RoundNo) -> bool {
    todo!("U2: may_close")
}
