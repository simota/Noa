//! The artifact links a verification records (spec `artifact-links`
//! L1-3, L1-5): at its start, the exact change it judges and where it
//! came from ([`links::record_handoff`]); when its report is written, its
//! verdict label. A link that cannot be recorded is told in a notice and
//! never fails the verification.

use std::path::{Path, PathBuf};

use verify::links::{self, ChangeId, Edit, Info, Millis, Node, Outcome};
use verify::model::types::{Head, Session};

/// Why a work-tree verification has no `Judged` link: the tree was at
/// one change before the capture and at another after it.
pub(super) const TREE_MOVED: &str =
    "取り込み中に作業ツリーが変わったため、対象の版を記録していません";

/// The links store of the repository whose main worktree is `main`, for
/// the verifications stored under `store_root` (`<data>/noa/verifications`):
/// `<data>/noa/links/<repo key of main>/links.jsonl`, which is
/// [`links::path`] without running git again (`main` is already the
/// first entry of `git worktree list`, as [`links::main_worktree`] takes).
pub(super) fn log_path(store_root: &Path, main: &Path) -> PathBuf {
    store_root
        .parent()
        .unwrap_or(store_root)
        .join("links")
        .join(links::repo_key(main))
        .join(links::LINKS_FILE)
}

/// The work tree's change read before and after the capture
/// ([`links::current_change`]).
pub(super) type Around = (Result<ChangeId, String>, Result<ChangeId, String>);

/// The change a verification judged, or why it names none.
///
/// - A ref head: the commit the capture took (`head_sha`).
/// - The work tree: its change when it was the same before and after the
///   capture and sits on the commit the capture took (`head_sha`);
///   [`TREE_MOVED`] otherwise (a tree at A before and after but at B while
///   captured: the result is never tied to a state it did not see); git's
///   reason when it could not be named.
pub(super) fn judged(
    head: &Head,
    head_sha: &str,
    around: Option<Around>,
) -> Result<ChangeId, String> {
    match (head, around) {
        (Head::Ref(_), _) => Ok(ChangeId::Commit(head_sha.to_owned())),
        (Head::Worktree, Some((Ok(before), Ok(after))))
            if before == after && before.head() == Some(head_sha) =>
        {
            Ok(before)
        }
        (Head::Worktree, Some((Ok(_), Ok(_)))) => Err(TREE_MOVED.to_owned()),
        (Head::Worktree, Some((Err(why), _) | (_, Err(why)))) => {
            Err(format!("対象の版を記録していません: {why}"))
        }
        (Head::Worktree, None) => Err("対象の版を記録していません".to_owned()),
    }
}

/// Records verification `id` (`events`: its log; `capture`: its snapshot
/// dir) as created on `change` in `workdir`. The notice to show: why the
/// change has no link, each link the store refused, or why none could be
/// written.
pub(super) fn record(
    log: &Path,
    id: &str,
    change: Result<ChangeId, String>,
    workdir: &Path,
    info: Info,
    capture: &Path,
) -> Option<String> {
    let (change, why) = match change {
        Ok(change) => (Some(change), None),
        Err(why) => (None, Some(why)),
    };
    let stored = match links::record_handoff(
        log,
        Node::Verify(id.to_owned()),
        change,
        workdir,
        info,
        Some(capture),
    ) {
        Ok(handoff) => handoff.notice(),
        Err(e) => Some(format!("リンクを記録できませんでした: {e}")),
    };
    join_notices(why, stored)
}

fn join_notices(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}。{b}")),
        (a, b) => a.or(b),
    }
}

/// The outcome recorded for a verification that has no verdict (any
/// more): one that a later round or the person's end left without one.
pub(super) const NO_VERDICT: &str = "判定なし";

/// What `session`'s report says it ended with: its verdict's label.
pub(super) fn outcome_label(session: &Session) -> Option<String> {
    session.verdict().map(|v| v.decision.label().to_owned())
}

/// The outcome the store holds for verification `id`; `None` when it
/// holds none or cannot be read.
pub(super) fn stored_outcome(log: &Path, id: &str) -> Option<String> {
    links::replay(log)
        .ok()?
        .info_of(&Node::Verify(id.to_owned()))
        .outcome
}

/// Records `label` as verification `id`'s outcome. `Err` is the notice.
pub(super) fn observe_outcome(
    log: &Path,
    id: &str,
    label: &str,
    now: Millis,
) -> Result<(), String> {
    let edit = Edit::Observe {
        node: Node::Verify(id.to_owned()),
        info: Info {
            outcome: Some(label.to_owned()),
            ..Info::default()
        },
    };
    match links::append(log, &[edit], now) {
        Ok(outcomes) => match outcomes.first() {
            Some(Outcome::Refused(why)) => {
                Err(format!("検証結果をリンクに記録できませんでした: {why}"))
            }
            _ => Ok(()),
        },
        Err(e) => Err(format!("検証結果をリンクに記録できませんでした: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(c: char) -> ChangeId {
        ChangeId::Commit(c.to_string().repeat(40))
    }

    #[test]
    fn a_ref_head_judges_the_captured_commit() {
        let sha = "a".repeat(40);
        assert_eq!(
            judged(&Head::Ref("main".into()), &sha, None),
            Ok(ChangeId::Commit(sha))
        );
    }

    #[test]
    fn the_work_tree_judges_its_change_only_when_it_held_still() {
        let a = "a".repeat(40);
        assert_eq!(
            judged(
                &Head::Worktree,
                &a,
                Some((Ok(commit('a')), Ok(commit('a'))))
            ),
            Ok(commit('a'))
        );
        let snap = ChangeId::Snapshot(format!("snap-{a}-{}", "0".repeat(16)));
        assert_eq!(
            judged(
                &Head::Worktree,
                &a,
                Some((Ok(snap.clone()), Ok(snap.clone())))
            ),
            Ok(snap)
        );
        assert_eq!(
            judged(
                &Head::Worktree,
                &a,
                Some((Ok(commit('a')), Ok(commit('b'))))
            ),
            Err(TREE_MOVED.to_owned())
        );
        let err = judged(
            &Head::Worktree,
            &a,
            Some((Ok(commit('a')), Err(links::TOO_LARGE.to_owned()))),
        )
        .unwrap_err();
        assert!(err.contains(links::TOO_LARGE), "{err}");
    }

    #[test]
    fn a_tree_that_was_elsewhere_while_captured_is_not_judged() {
        // At A before and after, but the capture took B.
        let b = "b".repeat(40);
        assert_eq!(
            judged(
                &Head::Worktree,
                &b,
                Some((Ok(commit('a')), Ok(commit('a'))))
            ),
            Err(TREE_MOVED.to_owned())
        );
        // A snapshot on A, the capture on B.
        let snap = ChangeId::Snapshot(format!("snap-{}-{}", "a".repeat(40), "0".repeat(16)));
        assert_eq!(
            judged(&Head::Worktree, &b, Some((Ok(snap.clone()), Ok(snap)))),
            Err(TREE_MOVED.to_owned())
        );
    }

    #[test]
    fn the_log_sits_beside_the_verifications_under_links() {
        assert_eq!(
            log_path(Path::new("/d/noa/verifications"), Path::new("/r/repo")),
            Path::new("/d/noa/links")
                .join(links::repo_key(Path::new("/r/repo")))
                .join("links.jsonl")
        );
    }
}
