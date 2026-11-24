//! The evidence standard the widget holds the judge to (L1-6, Judge
//! ruling): what each ruling needs, the verdict's consistency with the
//! rulings, the default cross-check assignments (L1-4 item 4) and when
//! round 1 may close early (L1-4 item 5). Pure.

use std::collections::BTreeMap;

use crate::model::types::{Defect, PartId, RoundNo, Ruling, Session, Subject, Verdict};

/// Why `ruling` on criterion `criterion` in `round` breaks the evidence
/// standard, as one problem line (`AC-2: verified needs a matched re-run or
/// two independent passes`).
pub fn check_ruling(
    _session: &Session,
    _round: RoundNo,
    _criterion: &str,
    _ruling: &Ruling,
) -> Result<(), String> {
    todo!("U2: rules::check_ruling")
}

/// Why `verdict` does not follow from `rulings` (this round's, every
/// criterion) and `defects` (as this round's defect ops leave them).
pub fn check_verdict(
    _session: &Session,
    _rulings: &BTreeMap<String, Ruling>,
    _defects: &[Defect],
    _verdict: &Verdict,
) -> Result<(), String> {
    todo!("U2: rules::check_verdict")
}

/// The assignments round `round` gets when the judge gave none: every
/// `fail` and every `pass` resting on a single verifier, each given to a
/// verifier other than its reporter, round-robin.
pub fn default_assignments(_session: &Session, _round: RoundNo) -> BTreeMap<PartId, Vec<Subject>> {
    todo!("U2: rules::default_assignments")
}

/// Whether round 1 may close early with `rulings`: every criterion
/// verified by two or more verifiers with re-runs that matched.
pub fn early_close_allowed(_session: &Session, _rulings: &BTreeMap<String, Ruling>) -> bool {
    todo!("U2: rules::early_close_allowed")
}
