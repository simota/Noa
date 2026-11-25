//! A new review's links (artifact-links L1-3, L1-5): the exact change it
//! judged, where it came from, and its verdict once it has one, recorded
//! in the repository's link store. A link that cannot be recorded never
//! fails the review: it becomes a notice.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use review::links::{self, ChangeId, Edit, Info, Node};
use review::model::types::{Head, Session, Target, TargetKind};
use review::store;

/// Why a work-tree review has no `Judged` link when the tree moved while
/// it was captured.
pub(super) const MOVED: &str = "取り込み中に作業ツリーが変わったため、対象の版を記録していません";

/// The outcome recorded for a review that has no verdict (any more).
pub(crate) const NO_VERDICT: &str = "判定なし";

/// The file in a review's store directory naming its link store and its
/// repository, written at hand-off: the store is found from it once the
/// work tree the review ran in is gone.
const STORE_FILE: &str = "links-store.json";

/// Where a review's links are: the link store and the main worktree of
/// the repository it is keyed by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Place {
    pub log: PathBuf,
    pub repo: PathBuf,
}

/// The link store of the repository `workdir` is in, beside the reviews
/// under `reviews_root` (`<data dir>/noa/reviews` → `<data dir>/noa/links`),
/// and that repository's main worktree.
pub(super) fn place_of(reviews_root: &Path, workdir: &Path) -> Result<Place, String> {
    let data = reviews_root.parent().ok_or_else(|| {
        format!(
            "{} の上にデータフォルダがありません",
            reviews_root.display()
        )
    })?;
    let repo = links::main_worktree(workdir)?;
    let log = data
        .join("links")
        .join(links::repo_key(&repo))
        .join(links::LINKS_FILE);
    Ok(Place { log, repo })
}

/// The link store of the repository `workdir` is in (see [`place_of`]).
pub(super) fn store_of(reviews_root: &Path, workdir: &Path) -> Result<PathBuf, String> {
    place_of(reviews_root, workdir).map(|place| place.log)
}

/// The place recorded in the review store directory `dir` at hand-off.
pub(crate) fn recorded(dir: &Path) -> Option<Place> {
    let text = fs::read(dir.join(STORE_FILE)).ok()?;
    serde_json::from_slice(&text).ok()
}

/// Records `place` in the review store directory `dir`.
fn remember(dir: &Path, place: &Place) -> Result<(), String> {
    let text = serde_json::to_vec(place).map_err(|e| e.to_string())?;
    fs::write(dir.join(STORE_FILE), text).map_err(|e| e.to_string())
}

/// Whether the change a review of `target` judges is read from its work
/// tree, before and after the capture ([`links::current_change`]).
pub(super) fn reads_work_tree(target: &Target) -> bool {
    target.kind == TargetKind::Git && target.head == Head::Worktree
}

/// The exact change a review of `target` judged, and the notice to give
/// when it has none to record. A ref is its captured commit (`head_sha`);
/// the work tree is its current change, taken `before` and `after` the
/// capture, recorded only when the two agree. A files or document review
/// judges no change.
pub(super) fn judged(
    target: &Target,
    head_sha: Option<&str>,
    before: Option<&Result<ChangeId, String>>,
    after: Option<&Result<ChangeId, String>>,
) -> (Option<ChangeId>, Option<String>) {
    if target.kind != TargetKind::Git {
        return (None, None);
    }
    match &target.head {
        Head::Ref(_) => (head_sha.map(|sha| ChangeId::Commit(sha.to_owned())), None),
        Head::Worktree => match (before, after) {
            (Some(Ok(before)), Some(Ok(after))) if before == after => (Some(before.clone()), None),
            (Some(Ok(_)), Some(Ok(_))) => (None, Some(MOVED.to_owned())),
            (Some(Err(why)), _) | (_, Some(Err(why))) => (
                None,
                Some(format!("対象の版を識別できないため記録していません: {why}")),
            ),
            _ => (None, None),
        },
    }
}

/// What a new review records: its node and info, the change it judged and
/// the work tree it ran in.
pub(crate) struct NewLinks<'a> {
    pub reviews_root: &'a Path,
    pub record: Node,
    pub dir: &'a Path,
    pub label: String,
    pub workdir: &'a Path,
    pub change: Option<ChangeId>,
    /// The snapshot's capture dir.
    pub capture: &'a Path,
    /// A git review: a store that cannot be found is said.
    pub git: bool,
}

/// Records a new review's hand-off (`links::record_handoff`). Returns the
/// store it went to, when there is one, and the notices: links the store
/// refused, or why nothing was recorded.
pub(crate) fn record(new: NewLinks<'_>) -> (Option<PathBuf>, Vec<String>) {
    let place = match place_of(new.reviews_root, new.workdir) {
        Ok(place) => place,
        // A files or document review outside any repository has no store.
        Err(_) if !new.git => return (None, Vec::new()),
        Err(why) => return (None, vec![format!("リンクを記録できませんでした: {why}")]),
    };
    let mut notices = Vec::new();
    if let Err(why) = remember(new.dir, &place) {
        notices.push(format!("リンクの保存先を記録できませんでした: {why}"));
    }
    let log = place.log;
    let info = Info {
        label: Some(new.label),
        record: Some(new.dir.join(store::EVENTS_FILE)),
        ..Info::default()
    };
    match links::record_handoff(
        &log,
        new.record,
        new.change,
        new.workdir,
        info,
        Some(new.capture),
    ) {
        Ok(handoff) => notices.extend(handoff.notice()),
        Err(e) => notices.push(format!("リンクを記録できませんでした: {e}")),
    }
    (Some(log), notices)
}

/// The outcome to observe for `session` when it is not `last` (the one in
/// the store): its verdict's label, or [`NO_VERDICT`] once a verdict that
/// was recorded is gone (a round added after it). With nothing recorded,
/// no verdict is nothing to record.
pub(super) fn outcome_change(session: &Session, last: Option<&str>) -> Option<String> {
    let label = match session.verdict() {
        Some(verdict) => verdict.decision.label(),
        None if last.is_none() => return None,
        None => NO_VERDICT,
    };
    (last != Some(label)).then(|| label.to_owned())
}

/// The outcome the store `log` holds for `record`, if any.
pub(super) fn recorded_outcome(log: &Path, record: &Node) -> Option<String> {
    links::replay(log).ok()?.info_of(record).outcome
}

/// Records `outcome` as `record`'s verdict in the store `log`.
pub(super) fn observe_outcome(log: &Path, record: Node, outcome: String) -> Result<(), String> {
    let edit = Edit::Observe {
        node: record,
        info: Info {
            outcome: Some(outcome),
            ..Info::default()
        },
    };
    links::append(log, &[edit], links::now_millis())
        .map(drop)
        .map_err(|e| format!("判定をリンクに記録できませんでした: {e}"))
}
