//! A task's related items (「関連成果」, `docs/specs/task-links.md` L1-5,
//! L1-6, L2-3): its runs, its change versions, the Review and Verify
//! sessions linked to it with the version each checked, and its PRs,
//! folded from the run log and the link log ([`crate::task_links`]).
//!
//! The surface is fixed by the spec (L2-2); the bodies are unit U2's.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::model::types::{Millis, POLL_MS, Run, RunId, TaskRef};
use crate::task_links::{self, LinkEvent, SessionKind, TaskKey};

/// One distinct head commit of a task's change: `v<no>`, numbered in the
/// order the link log first names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub no: u32,
    pub commit: String,
    pub run: RunId,
    pub seen: Millis,
}

/// A linked session's outcome: still running, a verdict (its decision
/// name), or ended without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Running,
    Decided(String),
    EndedWithout,
}

/// What a session checked (L1-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetVersion {
    Version { no: u32, uncommitted: bool },
    Base { uncommitted: bool },
    Unknown,
}

/// A Review or Verify session linked to the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub kind: SessionKind,
    pub session: String,
    pub run: RunId,
    pub target: TargetVersion,
    pub outcome: Outcome,
    pub at: Millis,
}

/// A PR linked by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrLink {
    pub id: String,
    pub url: String,
    pub number: Option<u64>,
    pub at: Millis,
}

/// Everything related to one task.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Related {
    pub runs: Vec<RunId>,
    pub versions: Vec<Version>,
    pub reviews: Vec<Checked>,
    pub verifications: Vec<Checked>,
    pub prs: Vec<PrLink>,
    /// The number of the version the head is on: the last `ChangeSeen`,
    /// or a version a session named first (`None` on the base).
    pub head: Option<u32>,
}

impl Related {
    /// The current version: the one the head was last seen on.
    pub fn current(&self) -> Option<&Version> {
        let no = self.head?;
        self.versions.iter().find(|v| v.no == no)
    }

    /// Whether some session of `kind` checked the current version itself
    /// (not uncommitted changes on it) (L1-5). A session still running
    /// counts; without a version there is nothing to check.
    pub fn registered(&self, kind: SessionKind) -> bool {
        let Some(current) = self.current() else {
            return false;
        };
        self.checked(kind).iter().any(|c| {
            c.target
                == TargetVersion::Version {
                    no: current.no,
                    uncommitted: false,
                }
        })
    }

    /// The reviews or the verifications.
    pub fn checked(&self, kind: SessionKind) -> &[Checked] {
        match kind {
            SessionKind::Review => &self.reviews,
            SessionKind::Verify => &self.verifications,
        }
    }

    fn checked_mut(&mut self, kind: SessionKind) -> &mut Vec<Checked> {
        match kind {
            SessionKind::Review => &mut self.reviews,
            SessionKind::Verify => &mut self.verifications,
        }
    }

    /// Whether nothing at all is related.
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
            && self.versions.is_empty()
            && self.reviews.is_empty()
            && self.verifications.is_empty()
            && self.prs.is_empty()
    }

    /// `commit` as a version of the task, pushed as the next one unless it
    /// is one already; its number.
    fn version_of(&mut self, commit: &str, run: &RunId, at: Millis) -> u32 {
        if let Some(v) = self.versions.iter().find(|v| v.commit == commit) {
            return v.no;
        }
        let no = self.versions.len() as u32 + 1;
        self.versions.push(Version {
            no,
            commit: commit.to_owned(),
            run: run.clone(),
            seen: at,
        });
        no
    }
}

/// The task a link event names, when it is one of Tasks'.
fn task_of(key: &TaskKey) -> Option<TaskRef> {
    serde_json::from_value(key.0.clone()).ok()
}

/// The `N` of a `…/pull/<N>` URL.
fn pr_number(url: &str) -> Option<u64> {
    let (_, rest) = url.rsplit_once("/pull/")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The related items of every task, from its runs and the link log (L2-3).
/// Reads no clock, no git and no file.
pub fn fold(runs: &[Run], events: &[LinkEvent]) -> BTreeMap<TaskRef, Related> {
    // 1. Ids are never reused: a removal holds wherever it is in the log.
    let removed: BTreeSet<(TaskRef, &str)> = events
        .iter()
        .filter_map(|event| match event {
            LinkEvent::LinkRemoved { task, id, .. } => Some((task_of(task)?, id.as_str())),
            _ => None,
        })
        .collect();
    // 2. The runs of each task, newest first.
    let mut out: BTreeMap<TaskRef, Related> = BTreeMap::new();
    let mut sorted: Vec<&Run> = runs.iter().collect();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.started));
    for run in &sorted {
        out.entry(run.task.clone())
            .or_default()
            .runs
            .push(run.run.clone());
    }
    let by_id: HashMap<&str, &Run> = runs.iter().map(|r| (r.run.as_str(), r)).collect();
    let mut bases: HashMap<&str, &str> = HashMap::new();
    // Each linked session's task, by (kind, session).
    let mut linked: HashMap<(SessionKind, &str), TaskRef> = HashMap::new();
    // Every (kind, session) whose first `SessionLinked` was taken, removed
    // or not.
    let mut sessions: HashSet<(SessionKind, &str)> = HashSet::new();
    // 3. The log in file order.
    for event in events {
        match event {
            LinkEvent::ChangeBase { run, commit, .. } => {
                if by_id.contains_key(run.as_str()) {
                    bases.entry(run.as_str()).or_insert(commit.as_str());
                }
            }
            LinkEvent::ChangeSeen { run, commit, at } => {
                let Some(found) = by_id.get(run.as_str()) else {
                    continue;
                };
                let related = out.entry(found.task.clone()).or_default();
                related.head = if bases.get(run.as_str()) == Some(&commit.as_str()) {
                    None
                } else {
                    Some(related.version_of(commit, &found.run, *at))
                };
            }
            LinkEvent::SessionLinked {
                kind,
                session,
                task,
                run,
                commit,
                uncommitted,
                at,
                ..
            } => {
                let Some(task) = task_of(task) else {
                    continue;
                };
                let Some(found) = by_id.get(run.as_str()) else {
                    continue;
                };
                if !sessions.insert((*kind, session.as_str())) {
                    continue;
                }
                let related = out.entry(task.clone()).or_default();
                let uncommitted = *uncommitted;
                // The commit is a version even when the link is removed:
                // 外す removes the link, never a version (L1-8, Decision 5).
                let target = match commit.as_deref() {
                    None => TargetVersion::Unknown,
                    Some(c) if bases.get(run.as_str()) == Some(&c) => {
                        TargetVersion::Base { uncommitted }
                    }
                    Some(c) => {
                        let known = related.versions.len();
                        let no = related.version_of(c, &found.run, *at);
                        if related.versions.len() > known {
                            related.head = Some(no);
                        }
                        TargetVersion::Version { no, uncommitted }
                    }
                };
                if removed.contains(&(task.clone(), session.as_str())) {
                    continue;
                }
                related.checked_mut(*kind).push(Checked {
                    kind: *kind,
                    session: session.clone(),
                    run: found.run.clone(),
                    target,
                    outcome: Outcome::Running,
                    at: *at,
                });
                linked.insert((*kind, session.as_str()), task);
            }
            LinkEvent::SessionOutcome {
                kind,
                session,
                decision,
                closed,
                ..
            } => {
                let Some(task) = linked.get(&(*kind, session.as_str())) else {
                    continue;
                };
                let outcome = match (decision, closed) {
                    (Some(name), _) => Outcome::Decided(name.clone()),
                    (None, true) => Outcome::EndedWithout,
                    (None, false) => Outcome::Running,
                };
                if let Some(checked) = out.get_mut(task).and_then(|r| {
                    r.checked_mut(*kind)
                        .iter_mut()
                        .find(|c| c.session == *session)
                }) {
                    checked.outcome = outcome;
                }
            }
            LinkEvent::PrLinked { id, task, url, at } => {
                let Some(task) = task_of(task) else {
                    continue;
                };
                if removed.contains(&(task.clone(), id.as_str())) {
                    continue;
                }
                out.entry(task).or_default().prs.push(PrLink {
                    id: id.clone(),
                    url: url.clone(),
                    number: pr_number(url),
                    at: *at,
                });
            }
            LinkEvent::Repo { .. } | LinkEvent::LinkRemoved { .. } => {}
        }
    }
    out
}

/// `vN`, `vN+未コミット`, `着手時点`, `着手時点+未コミット` or `版不明` (L1-5).
pub fn version_text(target: &TargetVersion) -> String {
    let dirty = |uncommitted: bool| if uncommitted { "+未コミット" } else { "" };
    match target {
        TargetVersion::Version { no, uncommitted } => format!("v{no}{}", dirty(*uncommitted)),
        TargetVersion::Base { uncommitted } => format!("着手時点{}", dirty(*uncommitted)),
        TargetVersion::Unknown => "版不明".to_owned(),
    }
}

/// The outcome's label for a session of `kind` (L1-5): Review's and
/// Verify's own words for their decisions (an unknown decision as
/// written), 進行中, or 判定なしで終了.
pub fn outcome_label(kind: SessionKind, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Running => "進行中".to_owned(),
        Outcome::EndedWithout => "判定なしで終了".to_owned(),
        Outcome::Decided(name) => {
            let label = match (kind, name.as_str()) {
                (SessionKind::Review, "approve") => "承認",
                (SessionKind::Review, "approve_with_fixes") => "修正後承認",
                (SessionKind::Review, "request_changes") => "要修正",
                (SessionKind::Review, "reject") => "差し戻し",
                (SessionKind::Verify, "pass") => "合格",
                (SessionKind::Verify, "pass_with_issues") => "条件付き合格",
                (SessionKind::Verify, "fail") => "不合格",
                (SessionKind::Verify, "inconclusive") => "判定不能",
                (_, other) => other,
            };
            label.to_owned()
        }
    }
}

/// What 関連を追加 accepts, for its refusals.
const PR_ACCEPTED: &str =
    "PR の URL（https://<host>/<owner>/<name>/pull/<番号>）か、#番号・番号を入力してください";

/// The PR URL to store for what the person typed (L1-8); `Err` says what is
/// accepted. `https://<host>/<owner>/<name>/pull/<N>` (anything after `<N>`
/// dropped), or `#N` / `N` on `gh_repo` (`owner/name`) at github.com.
pub fn parse_pr(input: &str, gh_repo: Option<&str>) -> Result<String, String> {
    let input = input.trim();
    let number = |digits: &str| -> Option<u64> {
        (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| digits.parse().ok())
            .flatten()
            .filter(|n| *n > 0)
    };
    if let Some(n) = number(input.strip_prefix('#').unwrap_or(input)) {
        let repo = gh_repo.ok_or_else(|| {
            "GitHub のリポジトリがわからないため番号では追加できません".to_owned()
        })?;
        return Ok(format!("https://github.com/{repo}/pull/{n}"));
    }
    let refused = || PR_ACCEPTED.to_owned();
    let rest = input.strip_prefix("https://").ok_or_else(refused)?;
    let mut parts = rest.splitn(5, '/');
    let (Some(host), Some(owner), Some(name), Some("pull"), Some(tail)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(refused());
    };
    let plain = |s: &str| !s.is_empty() && !s.contains(['?', '#', ' ']);
    if !plain(host) || !plain(owner) || !plain(name) {
        return Err(refused());
    }
    let end = tail.find(['/', '?', '#']).unwrap_or(tail.len());
    let n = number(&tail[..end]).ok_or_else(refused)?;
    Ok(format!("https://{host}/{owner}/{name}/pull/{n}"))
}

/// The link log, re-read when its (len, mtime) changed, at most every
/// `POLL_MS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkScan {
    log: PathBuf,
    stat: Option<(u64, SystemTime)>,
    checked: Option<Millis>,
}

impl LinkScan {
    pub fn new(log: PathBuf) -> Self {
        LinkScan {
            log,
            stat: None,
            checked: None,
        }
    }

    /// The log's events when it changed since the last poll (always on the
    /// first), else `None`. A log that cannot be read is taken as
    /// unchanged and tried again at the next poll that is due.
    pub fn poll(&mut self, now: Millis) -> Option<Vec<LinkEvent>> {
        let first = self.checked.is_none();
        if self
            .checked
            .is_some_and(|at| now < at.saturating_add(POLL_MS))
        {
            return None;
        }
        self.checked = Some(now);
        let stat = std::fs::metadata(&self.log)
            .ok()
            .and_then(|m| Some((m.len(), m.modified().ok()?)));
        if !first && stat == self.stat {
            return None;
        }
        let events = task_links::read(&self.log).ok()?;
        self.stat = stat;
        Some(events)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::types::AgentKind;
    use crate::task_links::MatchedBy;

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 40).collect()
    }

    fn key(task: &TaskRef) -> TaskKey {
        TaskKey(serde_json::to_value(task).unwrap())
    }

    fn local(id: &str) -> TaskRef {
        TaskRef::Local(id.into())
    }

    fn issue(n: u64) -> TaskRef {
        TaskRef::GitHub {
            repo: "o/n".into(),
            number: n,
        }
    }

    fn run(id: &str, task: &TaskRef, started: Millis, branch: bool) -> Run {
        Run {
            run: RunId::new(id),
            task: task.clone(),
            agent: AgentKind::Claude,
            workdir: PathBuf::from(format!("/src/noa-wt/task-{id}")),
            branch: branch.then(|| format!("task-{id}")),
            pane: None,
            board: None,
            started,
            closed: None,
        }
    }

    fn base(run: &str, commit: &str, at: Millis) -> LinkEvent {
        LinkEvent::ChangeBase {
            run: run.into(),
            commit: commit.into(),
            at,
        }
    }

    fn seen(run: &str, commit: &str, at: Millis) -> LinkEvent {
        LinkEvent::ChangeSeen {
            run: run.into(),
            commit: commit.into(),
            at,
        }
    }

    fn linked(
        kind: SessionKind,
        session: &str,
        task: &TaskRef,
        run: &str,
        commit: Option<&str>,
        uncommitted: bool,
        at: Millis,
    ) -> LinkEvent {
        LinkEvent::SessionLinked {
            kind,
            session: session.into(),
            task: key(task),
            run: run.into(),
            commit: commit.map(str::to_owned),
            uncommitted,
            by: MatchedBy::Worktree,
            at,
        }
    }

    fn outcome(
        kind: SessionKind,
        session: &str,
        decision: Option<&str>,
        closed: bool,
    ) -> LinkEvent {
        LinkEvent::SessionOutcome {
            kind,
            session: session.into(),
            decision: decision.map(str::to_owned),
            closed,
            at: 50,
        }
    }

    fn pr(id: &str, task: &TaskRef, url: &str, at: Millis) -> LinkEvent {
        LinkEvent::PrLinked {
            id: id.into(),
            task: key(task),
            url: url.into(),
            at,
        }
    }

    fn removed(task: &TaskRef, id: &str) -> LinkEvent {
        LinkEvent::LinkRemoved {
            task: key(task),
            id: id.into(),
            at: 99,
        }
    }

    /// The L0 picture: one run, v1 then v2; a verification on v1 (pass),
    /// a review on v2 (approve); a PR.
    fn l0() -> (Vec<Run>, Vec<LinkEvent>) {
        let t = local("t-1");
        let runs = vec![run("run-5", &t, 1, true)];
        let events = vec![
            LinkEvent::Repo {
                path: "/src/noa".into(),
                at: 1,
            },
            base("run-5", &sha('0'), 2),
            seen("run-5", &sha('1'), 3),
            linked(
                SessionKind::Verify,
                "ver-1",
                &t,
                "run-5",
                Some(&sha('1')),
                false,
                4,
            ),
            outcome(SessionKind::Verify, "ver-1", Some("pass"), true),
            seen("run-5", &sha('2'), 5),
            linked(
                SessionKind::Review,
                "rev-1",
                &t,
                "run-5",
                Some(&sha('2')),
                false,
                6,
            ),
            outcome(SessionKind::Review, "rev-1", Some("approve"), true),
            pr("pr-7", &t, "https://github.com/o/n/pull/45", 7),
        ];
        (runs, events)
    }

    #[test]
    fn links_ac1_fold_gives_each_related_item_once() {
        let (runs, events) = l0();
        let all = fold(&runs, &events);
        assert_eq!(all.len(), 1);
        let r = &all[&local("t-1")];
        assert_eq!(r.runs, [RunId::new("run-5")]);
        assert_eq!(
            r.versions,
            [
                Version {
                    no: 1,
                    commit: sha('1'),
                    run: RunId::new("run-5"),
                    seen: 3
                },
                Version {
                    no: 2,
                    commit: sha('2'),
                    run: RunId::new("run-5"),
                    seen: 5
                },
            ]
        );
        assert_eq!(
            r.reviews,
            [Checked {
                kind: SessionKind::Review,
                session: "rev-1".into(),
                run: RunId::new("run-5"),
                target: TargetVersion::Version {
                    no: 2,
                    uncommitted: false
                },
                outcome: Outcome::Decided("approve".into()),
                at: 6,
            }]
        );
        assert_eq!(r.verifications.len(), 1);
        assert_eq!(r.verifications[0].session, "ver-1");
        assert_eq!(
            r.prs,
            [PrLink {
                id: "pr-7".into(),
                url: "https://github.com/o/n/pull/45".into(),
                number: Some(45),
                at: 7,
            }]
        );
        // The same events again (a second reader) change nothing; a
        // repeated link line of one session is not a second item.
        let mut twice = events.clone();
        twice.push(seen("run-5", &sha('2'), 8));
        twice.push(linked(
            SessionKind::Review,
            "rev-1",
            &local("t-1"),
            "run-5",
            Some(&sha('1')),
            false,
            9,
        ));
        assert_eq!(fold(&runs, &twice), all);
    }

    #[test]
    fn links_ac4_current_v2_review_v2_verify_v1() {
        let (runs, events) = l0();
        let r = &fold(&runs, &events)[&local("t-1")];
        assert_eq!(r.current().map(|v| v.no), Some(2));
        assert!(r.registered(SessionKind::Review));
        assert!(!r.registered(SessionKind::Verify));
        assert_eq!(version_text(&r.reviews[0].target), "v2");
        assert_eq!(version_text(&r.verifications[0].target), "v1");
        assert_eq!(
            outcome_label(SessionKind::Review, &r.reviews[0].outcome),
            "承認"
        );
        assert_eq!(
            outcome_label(SessionKind::Verify, &r.verifications[0].outcome),
            "合格"
        );
    }

    #[test]
    fn links_ac4_head_back_on_an_earlier_version_is_current() {
        let t = local("t-1");
        let runs = vec![run("run-5", &t, 1, true)];
        let mut events = vec![
            base("run-5", &sha('0'), 2),
            seen("run-5", &sha('1'), 3),
            seen("run-5", &sha('2'), 4),
            // `git reset --hard` back to v1, then a verification of it.
            seen("run-5", &sha('1'), 5),
            linked(
                SessionKind::Verify,
                "ver-1",
                &t,
                "run-5",
                Some(&sha('1')),
                false,
                6,
            ),
            linked(
                SessionKind::Review,
                "rev-1",
                &t,
                "run-5",
                Some(&sha('2')),
                false,
                7,
            ),
        ];
        let r = &fold(&runs, &events)[&t];
        assert_eq!(r.versions.len(), 2, "no version renumbered");
        assert_eq!(r.current().map(|v| v.no), Some(1));
        assert!(r.registered(SessionKind::Verify));
        assert!(
            !r.registered(SessionKind::Review),
            "v2 is no longer current"
        );
        // Back on the base: no version is current, nothing is 未登録.
        events.push(seen("run-5", &sha('0'), 8));
        let r = &fold(&runs, &events)[&t];
        assert_eq!(r.current(), None);
        // A session naming a commit no poll saw yet: that is the new head.
        events.push(linked(
            SessionKind::Review,
            "rev-2",
            &t,
            "run-5",
            Some(&sha('3')),
            false,
            9,
        ));
        let r = &fold(&runs, &events)[&t];
        assert_eq!(r.current().map(|v| v.no), Some(3));
    }

    #[test]
    fn links_ac4_uncommitted_base_and_unknown_targets() {
        let t = local("t-1");
        let runs = vec![run("run-5", &t, 1, true)];
        let events = vec![
            base("run-5", &sha('0'), 2),
            seen("run-5", &sha('1'), 3),
            seen("run-5", &sha('2'), 4),
            linked(
                SessionKind::Verify,
                "ver-1",
                &t,
                "run-5",
                Some(&sha('2')),
                true,
                5,
            ),
            linked(
                SessionKind::Review,
                "rev-1",
                &t,
                "run-5",
                Some(&sha('0')),
                false,
                6,
            ),
            linked(
                SessionKind::Review,
                "rev-2",
                &t,
                "run-5",
                Some(&sha('0')),
                true,
                7,
            ),
            linked(SessionKind::Review, "rev-3", &t, "run-5", None, false, 8),
        ];
        let r = &fold(&runs, &events)[&t];
        // v2 checked with uncommitted changes on it: not v2 itself.
        assert_eq!(version_text(&r.verifications[0].target), "v2+未コミット");
        assert!(!r.registered(SessionKind::Verify));
        // The run's base is not a version.
        assert_eq!(r.versions.len(), 2);
        assert_eq!(version_text(&r.reviews[0].target), "着手時点");
        assert_eq!(version_text(&r.reviews[1].target), "着手時点+未コミット");
        assert_eq!(version_text(&r.reviews[2].target), "版不明");
        assert!(!r.registered(SessionKind::Review));
    }

    #[test]
    fn links_ac4_a_session_on_an_unseen_commit_makes_it_a_version_and_outcomes_take_the_last() {
        let t = local("t-1");
        let runs = vec![run("run-5", &t, 1, true)];
        let events = vec![
            base("run-5", &sha('0'), 2),
            seen("run-5", &sha('1'), 3),
            linked(
                SessionKind::Review,
                "rev-1",
                &t,
                "run-5",
                Some(&sha('9')),
                false,
                4,
            ),
            outcome(SessionKind::Review, "rev-1", Some("request_changes"), false),
            outcome(SessionKind::Review, "rev-1", None, true),
            linked(
                SessionKind::Verify,
                "ver-1",
                &t,
                "run-5",
                Some(&sha('9')),
                false,
                5,
            ),
            outcome(SessionKind::Verify, "ver-1", Some("odd_new_name"), false),
            // An outcome of a session never linked is nothing.
            outcome(SessionKind::Verify, "ver-x", Some("pass"), true),
        ];
        let r = &fold(&runs, &events)[&t];
        assert_eq!(
            r.current().map(|v| (v.no, v.commit.clone())),
            Some((2, sha('9')))
        );
        assert!(
            r.registered(SessionKind::Review),
            "a review on v2 is registered"
        );
        assert_eq!(r.reviews[0].outcome, Outcome::EndedWithout);
        assert_eq!(
            outcome_label(SessionKind::Review, &r.reviews[0].outcome),
            "判定なしで終了"
        );
        assert_eq!(
            outcome_label(SessionKind::Verify, &r.verifications[0].outcome),
            "odd_new_name"
        );
        assert_eq!(
            outcome_label(SessionKind::Verify, &Outcome::Running),
            "進行中"
        );
        assert_eq!(r.verifications.len(), 1);
    }

    #[test]
    fn links_ac6_parse_pr_accepts_urls_and_numbers_and_refuses_the_rest() {
        let o = Some("o/n");
        let url = "https://github.com/o/n/pull/45";
        assert_eq!(parse_pr(url, o).as_deref(), Ok(url));
        assert_eq!(parse_pr(url, None).as_deref(), Ok(url));
        assert_eq!(parse_pr(&format!("{url}/files"), o).as_deref(), Ok(url));
        assert_eq!(
            parse_pr(&format!(" {url}#discussion "), o).as_deref(),
            Ok(url)
        );
        assert_eq!(
            parse_pr("https://git.example.com/a/b/pull/7?x=1", None).as_deref(),
            Ok("https://git.example.com/a/b/pull/7")
        );
        assert_eq!(parse_pr("#45", o).as_deref(), Ok(url));
        assert_eq!(parse_pr("45", o).as_deref(), Ok(url));
        let no_repo = parse_pr("#45", None).unwrap_err();
        assert!(no_repo.contains("番号では追加できません"), "{no_repo}");
        assert_eq!(parse_pr("45", None).unwrap_err(), no_repo);
        for bad in [
            "https://github.com/o/n/issues/45",
            "http://github.com/o/n/pull/45",
            "",
            "   ",
            "#",
            "#0",
            "pull/45",
            "https://github.com/o/n/pull/",
            "https://github.com/o/n/pull/45x",
            "https://github.com//n/pull/45",
        ] {
            let why = parse_pr(bad, o).unwrap_err();
            assert!(why.contains("PR の URL"), "{bad:?}: {why}");
        }
    }

    #[test]
    fn links_ac6_removed_links_are_gone_and_a_pr_added_again_shows_once() {
        let t = local("t-1");
        let runs = vec![run("run-5", &t, 1, true)];
        let url = "https://github.com/o/n/pull/45";
        let events = vec![
            seen("run-5", &sha('1'), 2),
            linked(
                SessionKind::Review,
                "rev-1",
                &t,
                "run-5",
                Some(&sha('1')),
                false,
                3,
            ),
            pr("pr-4", &t, url, 4),
            removed(&t, "pr-4"),
            removed(&t, "rev-1"),
            pr("pr-6", &t, url, 6),
        ];
        let r = &fold(&runs, &events)[&t];
        assert!(r.reviews.is_empty());
        assert!(!r.registered(SessionKind::Review));
        assert_eq!(r.prs.len(), 1);
        assert_eq!(r.prs[0].id, "pr-6");
        // The version the removed review named stays: it is a fact of git.
        assert_eq!(r.versions.len(), 1);
        // A removal names the task: on another task the same id is not removed.
        let other = vec![pr("pr-4", &issue(12), url, 4), removed(&t, "pr-4")];
        assert_eq!(fold(&runs, &other)[&issue(12)].prs.len(), 1);
    }

    /// 外す on a review removes the link, not the version its commit made:
    /// a verification on a later version keeps its number, in the fold and
    /// after the log is read back.
    #[test]
    fn links_ac6_removing_a_review_keeps_the_versions_it_made() {
        let t = local("t-1");
        let runs = vec![run("run-5", &t, 1, true)];
        let (a, b) = (sha('a'), sha('b'));
        let mut events = vec![
            base("run-5", &sha('0'), 1),
            // A review records A before the change poll has seen it: v1.
            linked(
                SessionKind::Review,
                "rev-1",
                &t,
                "run-5",
                Some(&a),
                false,
                2,
            ),
            // The poll records B: v2.
            seen("run-5", &b, 3),
            linked(
                SessionKind::Verify,
                "ver-1",
                &t,
                "run-5",
                Some(&b),
                false,
                4,
            ),
            outcome(SessionKind::Verify, "ver-1", Some("pass"), true),
        ];
        let on_v2 = |r: &Related| {
            let versions: Vec<(u32, &str)> = r
                .versions
                .iter()
                .map(|v| (v.no, v.commit.as_str()))
                .collect();
            assert_eq!(versions, [(1, a.as_str()), (2, b.as_str())]);
            assert_eq!(r.verifications.len(), 1);
            let ver = &r.verifications[0];
            assert_eq!(
                ver.target,
                TargetVersion::Version {
                    no: 2,
                    uncommitted: false
                }
            );
            assert_eq!(
                format!(
                    "{} · {}",
                    version_text(&ver.target),
                    outcome_label(SessionKind::Verify, &ver.outcome)
                ),
                "v2 · 合格"
            );
            assert!(r.registered(SessionKind::Verify));
        };
        let before = &fold(&runs, &events)[&t];
        on_v2(before);
        assert_eq!(before.reviews.len(), 1);

        events.push(removed(&t, "rev-1"));
        let after = &fold(&runs, &events)[&t];
        on_v2(after);
        assert!(after.reviews.is_empty());

        let dir = std::env::temp_dir().join(format!("noa-tasks-removed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = task_links::links_log(&dir);
        for event in &events {
            task_links::append(&log, event).unwrap();
        }
        let read = task_links::read(&log).unwrap();
        assert_eq!(read, events);
        let reloaded = &fold(&runs, &read)[&t];
        on_v2(reloaded);
        assert_eq!(reloaded, after);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn links_ac7_versions_span_runs_and_one_pr_shows_on_two_tasks() {
        let t = local("t-1");
        let runs = vec![
            run("run-1", &t, 1, true),
            run("run-2", &t, 10, true),
            run("run-x", &issue(12), 20, false),
        ];
        let url = "https://github.com/o/n/pull/45";
        let events = vec![
            base("run-1", &sha('0'), 1),
            seen("run-1", &sha('1'), 2),
            seen("run-1", &sha('2'), 3),
            base("run-2", &sha('2'), 10),
            seen("run-2", &sha('3'), 11),
            pr("pr-1", &t, url, 12),
            pr("pr-2", &issue(12), url, 13),
            removed(&t, "pr-1"),
            // A line of a run not in the run log is skipped.
            seen("run-gone", &sha('8'), 14),
        ];
        let all = fold(&runs, &events);
        let r = &all[&t];
        assert_eq!(r.runs, [RunId::new("run-2"), RunId::new("run-1")]);
        let versions: Vec<(u32, String, &str)> = r
            .versions
            .iter()
            .map(|v| (v.no, v.commit.clone(), v.run.as_str()))
            .collect();
        assert_eq!(
            versions,
            [
                (1, sha('1'), "run-1"),
                (2, sha('2'), "run-1"),
                (3, sha('3'), "run-2")
            ]
        );
        assert!(r.prs.is_empty());
        assert_eq!(all[&issue(12)].prs[0].id, "pr-2");
        assert_eq!(all[&issue(12)].runs, [RunId::new("run-x")]);
    }

    #[test]
    fn links_ac2_link_scan_rereads_only_a_changed_log_and_at_most_every_poll() {
        let dir = std::env::temp_dir().join(format!("noa-tasks-linkscan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = task_links::links_log(&dir);
        let mut scan = LinkScan::new(log.clone());
        assert_eq!(scan.poll(0), Some(Vec::new()), "the first poll reads");
        assert_eq!(scan.poll(POLL_MS), None, "unchanged");
        let event = seen("run-5", &sha('1'), 1);
        task_links::append(&log, &event).unwrap();
        assert_eq!(scan.poll(POLL_MS + 1), None, "not due yet");
        assert_eq!(scan.poll(2 * POLL_MS), Some(vec![event]));
        assert_eq!(scan.poll(3 * POLL_MS), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
