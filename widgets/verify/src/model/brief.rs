//! The briefs agents get (L2-4), in English with constant section
//! headings, and the note that sends a turned-down file back.

use std::collections::BTreeMap;
use std::path::Path;

use crate::model::types::{Brief, EvidenceKey, PartId, RoundNo, Session};

/// The headings of the judge's criteria brief, in order.
pub const CRITERIA_SECTIONS: [&str; 6] = [
    "## 1. Verification request",
    "## 2. Target",
    "## 3. Spec",
    "## 4. Criteria the person gave",
    "## 5. How to write criteria",
    "## 6. Your output",
];

/// The headings of a round-1 verifier brief, in order.
pub const BLIND_SECTIONS: [&str; 8] = [
    "## 1. Request and round",
    "## 2. Target and your worktree",
    "## 3. Criteria",
    "## 4. Your strategy",
    "## 5. Evidence",
    "## 6. Rules",
    "## 7. User interventions",
    "## 8. How to reply",
];

/// The headings of a cross-check (round ≥ 2) verifier brief, in order.
pub const CROSS_SECTIONS: [&str; 9] = [
    "## 1. Request and round",
    "## 2. Target and your worktree",
    "## 3. Criteria",
    "## 4. Your strategy",
    "## 5. Rulings and results so far",
    "## 6. Your assignments",
    "## 7. Evidence",
    "## 8. User interventions",
    "## 9. How to reply",
];

/// The headings of the judge's ruling brief, in order.
pub const RULING_SECTIONS: [&str; 9] = [
    "## 1. Request and round",
    "## 2. Criteria",
    "## 3. Panel and receipts",
    "## 4. Results this round",
    "## 5. Cross-checks",
    "## 6. Defects",
    "## 7. Evidence standard",
    "## 8. This round",
    "## 9. Your output",
];

/// The only heading of a re-run brief.
pub const RERUN_SECTIONS: [&str; 1] = ["## Re-run"];

/// The judge's criteria brief; it writes `reply_path` (`c/criteria.json`
/// under `reply_dir`, the judge's reply dir).
pub fn criteria_brief(_session: &Session, _reply_dir: &Path, _reply_path: &Path) -> Brief {
    todo!("U2: criteria_brief")
}

/// Verifier `to`'s brief for `round`: blind in round 1, cross-checks
/// after. `reply_dir` is the verifier's reply dir, `worktree` where it
/// runs ([`crate::store::verifier_root`]).
pub fn verifier_brief(
    _session: &Session,
    _to: &PartId,
    _round: RoundNo,
    _reply_dir: &Path,
    _reply_path: &Path,
    _worktree: &Path,
) -> Brief {
    todo!("U2: verifier_brief")
}

/// The judge's ruling brief for `round`. `tails` holds the last 40 lines
/// of each failing command's stored log, read by the runner.
pub fn judge_brief(
    _session: &Session,
    _round: RoundNo,
    _reply_dir: &Path,
    _reply_path: &Path,
    _tails: &BTreeMap<EvidenceKey, String>,
) -> Brief {
    todo!("U2: judge_brief")
}

/// Verifier `to`'s re-run brief for `round`: the one command line that
/// starts `script` (absolute) detached or in the foreground
/// ([`crate::rerun::detached`]), the reminder not to edit `rerun_dir` or
/// the worktree, and "reply nothing".
pub fn rerun_brief(
    _session: &Session,
    _to: &PartId,
    _round: RoundNo,
    _script: &Path,
    _rerun_dir: &Path,
) -> Brief {
    todo!("U2: rerun_brief")
}

/// What an agent whose file was turned down is sent: the problems and
/// where to write the whole file again.
pub fn correction_note(_to: &PartId, _error: &str, _reply_path: &Path) -> String {
    todo!("U2: correction_note")
}
