//! The verification as a Markdown report (L1-8), for the clipboard, a
//! saved file and `<session dir>/report.md`, and the task 修正を依頼 hands
//! an agent (L1-7 item 3).

use std::collections::BTreeMap;
use std::path::Path;

use crate::model::types::{EvidenceKey, Session};

/// The report's section headings, in order (after the title).
pub const SECTIONS: [&str; 8] = [
    "## 総合判定",
    "## 受け入れ基準",
    "## 不具合",
    "## 追試",
    "## 証跡一覧",
    "## 基準の変更",
    "## 検証の欠落",
    "## ラウンド記録",
];

/// The report. Media are linked by absolute path into `base` (the judge's
/// reply dir) when given, by their path relative to it when `None` (証跡ごと
/// 書き出す).
pub fn report_markdown(_session: &Session, _base: Option<&Path>) -> String {
    todo!("U2: report_markdown")
}

/// Every brief that gave up: `(round, participant, why)`.
pub fn failures(_session: &Session) -> Vec<(u32, String, String)> {
    todo!("U2: failures")
}

/// The task for an agent asked to fix `criteria` (failed criterion ids)
/// and `defects` (defect ids): each failed criterion with its failing
/// evidence and `tails` (the last 80 lines of each cited stored log, read
/// by the runner), each defect with its repro steps.
pub fn fix_task(
    _session: &Session,
    _criteria: &[String],
    _defects: &[String],
    _tails: &BTreeMap<EvidenceKey, String>,
) -> String {
    todo!("U2: fix_task")
}
