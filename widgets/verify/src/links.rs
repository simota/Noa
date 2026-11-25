//! Artifact links (`docs/specs/artifact-links.md`): the agent runs that
//! worked on a task, the changes they produced (commits, snapshots of
//! uncommitted work) and the reviews and verifications that judged each
//! exact change, kept in one append-only store per repository:
//!
//! ```text
//! <data dir>/noa/links/<repo-key>/
//!   links.jsonl        one LinkEvent per line (append-only)
//!   links.jsonl.lock   the writers' flock
//! ```
//!
//! `<repo-key>` is the key of the repository's **main** worktree, so every
//! linked worktree of it shares one store, and removing a worktree removes
//! nothing here.
//!
//! This file is byte-identical in `widgets/tasks`, `widgets/review` and
//! `widgets/verify` (no widget depends on another): it names nothing else
//! of its crate, and a test fails when the three copies differ.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch.
pub type Millis = u64;

/// The store's file name under `<root>/<repo-key>/`.
pub const LINKS_FILE: &str = "links.jsonl";

/// Diff and untracked bytes together beyond which a work tree's state gets
/// no snapshot id (verify's `worktree.patch` cap).
pub const SNAPSHOT_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Why a work tree over [`SNAPSHOT_MAX_BYTES`] has no snapshot id.
pub const TOO_LARGE: &str = "大きすぎて識別できません";

/// Why a manual link that already exists is refused.
pub const ALREADY_LINKED: &str = "既にリンクされています";

/// Why a `Judged` link is refused while its review or verification is
/// judged on another change: a result is about one exact change.
pub const JUDGED_ELSEWHERE: &str = "この結果は既に別の版にリンクされています";

/// Why a work tree whose submodule has changes of its own, or that holds
/// an untracked repository, has no snapshot id: the diff names only a
/// commit for them, not what is inside.
const NESTED: &str = "サブモジュールか入れ子のリポジトリに変更があるため識別できません";

/// Why a work tree whose `HEAD` moved while its state was read has no
/// snapshot id.
const HEAD_MOVED: &str = "読み取り中に HEAD が動いたため識別できません";

// ------------------------------------------------------------------ ids

/// A task, with the same serde shape as the tasks widget's `TaskRef`
/// (`{"local":"t-1"}`, `{"github":{"repo":"o/n","number":12}}`,
/// `{"code":{"path":"…","key":"…"}}`), so Review and Verify can name one
/// without depending on Tasks.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskKey {
    Local(String),
    GitHub { repo: String, number: u64 },
    Code { path: String, key: String },
}

impl TaskKey {
    /// What `tasks --select=` takes for it: `t-…`, `#N`, or the marker's path.
    pub fn select(&self) -> String {
        match self {
            TaskKey::Local(id) => id.clone(),
            TaskKey::GitHub { number, .. } => format!("#{number}"),
            TaskKey::Code { path, .. } => path.clone(),
        }
    }
}

/// One exact state of code: a commit (its full SHA), or a snapshot of
/// uncommitted work on a commit (`snap-<head sha>-<16 hex>`, [`snapshot_id`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeId {
    Commit(String),
    Snapshot(String),
}

impl ChangeId {
    /// The SHA or the snapshot id.
    pub fn as_str(&self) -> &str {
        match self {
            ChangeId::Commit(id) | ChangeId::Snapshot(id) => id,
        }
    }

    /// Whether it has its kind's form: a commit is a full SHA-1 or SHA-256
    /// in lowercase hex, a snapshot `snap-<such a SHA>-<16 lowercase hex>`.
    /// The store skips a line that names one without it.
    pub fn is_valid(&self) -> bool {
        match self {
            ChangeId::Commit(sha) => is_sha(sha),
            ChangeId::Snapshot(id) => snapshot_parts(id).is_some(),
        }
    }

    /// The commit it is or sits on; `None` for a snapshot id not of the
    /// `snap-<head>-<16 hex>` form.
    pub fn head(&self) -> Option<&str> {
        match self {
            ChangeId::Commit(sha) => Some(sha),
            ChangeId::Snapshot(id) => snapshot_parts(id).map(|(head, _)| head),
        }
    }

    /// How a row names it: `abc1234`, or `abc1234 + 未コミット (snap 0123abcd)`.
    pub fn short(&self) -> String {
        match self {
            ChangeId::Commit(sha) => short_sha(sha).to_owned(),
            ChangeId::Snapshot(id) => match snapshot_parts(id) {
                Some((head, hex)) => {
                    format!("{} + 未コミット (snap {})", short_sha(head), &hex[..8])
                }
                None => id.clone(),
            },
        }
    }
}

/// Its first seven characters (cut on a character, not a byte: an id
/// made by hand may not be ASCII).
fn short_sha(sha: &str) -> &str {
    sha.char_indices()
        .nth(7)
        .map_or(sha, |(end, _)| &sha[..end])
}

/// `snap-<head>-<16 hex>` → `(head, hex)`.
fn snapshot_parts(id: &str) -> Option<(&str, &str)> {
    let (head, hex) = id.strip_prefix("snap-")?.rsplit_once('-')?;
    (is_sha(head) && hex.len() == 16 && is_hex(hex)).then_some((head, hex))
}

/// A full SHA-1 or SHA-256 object id in lowercase hex.
fn is_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && is_hex(s)
}

/// Lowercase hex digits only.
fn is_hex(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A record a link points at, by the id the record already has.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Node {
    Task(TaskKey),
    /// `run-<millis>[-n]`.
    Run(String),
    Change(ChangeId),
    /// `rev-<millis>[-n]`.
    Review(String),
    /// `ver-<millis>[-n]`.
    Verify(String),
}

impl Node {
    /// Whether the change it names, if any, has its kind's form
    /// ([`ChangeId::is_valid`]).
    fn is_valid(&self) -> bool {
        match self {
            Node::Change(change) => change.is_valid(),
            _ => true,
        }
    }
}

/// What a link says; each fixes the kinds it may join ([`Rel::allows`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rel {
    /// Task → Run.
    Ran,
    /// Run | Task → Change.
    Produced,
    /// Review | Verify → Change: the exact change the result is about.
    Judged,
    /// Review | Verify → Task | Run: where it came from.
    About,
}

impl Rel {
    /// Whether `from → to` is a pair this relation may join.
    pub fn allows(self, from: &Node, to: &Node) -> bool {
        let result = matches!(from, Node::Review(_) | Node::Verify(_));
        match self {
            Rel::Ran => matches!((from, to), (Node::Task(_), Node::Run(_))),
            Rel::Produced => {
                matches!(from, Node::Run(_) | Node::Task(_)) && matches!(to, Node::Change(_))
            }
            Rel::Judged => result && matches!(to, Node::Change(_)),
            Rel::About => result && matches!(to, Node::Task(_) | Node::Run(_)),
        }
    }
}

/// Who made a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Auto,
    Manual,
}

/// `lnk-<millis>[-n]`, unique in the store ([`new_id`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LinkId(String);

impl LinkId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LinkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a node was when last seen; the latest `Observed` wins per field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    /// Task title, run `claude · task-run-5`, review target label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The file or dir whose existence is "found": a task's file, a run's
    /// runs log, a snapshot's capture dir, a session's `events.jsonl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<PathBuf>,
    /// A run's work tree; a snapshot's work tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
    /// A run's branch (`None` for a run in the repository itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// A run: the commit it started on; a snapshot: its head commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// A review's or verification's verdict label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

impl Info {
    /// Takes every field `newer` has; whether anything changed.
    pub fn merge(&mut self, newer: &Info) -> bool {
        fn take<T: Clone + PartialEq>(slot: &mut Option<T>, newer: &Option<T>) -> bool {
            match newer {
                Some(value) if slot.as_ref() != Some(value) => {
                    *slot = Some(value.clone());
                    true
                }
                _ => false,
            }
        }
        // Every field, not the first that changed.
        let changed = [
            take(&mut self.label, &newer.label),
            take(&mut self.record, &newer.record),
            take(&mut self.workdir, &newer.workdir),
            take(&mut self.branch, &newer.branch),
            take(&mut self.head, &newer.head),
            take(&mut self.outcome, &newer.outcome),
        ];
        changed.contains(&true)
    }
}

/// One line of `links.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum LinkEvent {
    Linked {
        id: LinkId,
        from: Node,
        to: Node,
        rel: Rel,
        origin: Origin,
        at: Millis,
    },
    Unlinked {
        id: LinkId,
        at: Millis,
    },
    Observed {
        node: Node,
        info: Info,
        at: Millis,
    },
}

/// A live link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub id: LinkId,
    pub from: Node,
    pub to: Node,
    pub rel: Rel,
    pub origin: Origin,
    pub at: Millis,
}

/// `(from, to, rel)`: what dedupe compares.
pub type Triple = (Node, Node, Rel);

/// The store folded: the live links in the order they were linked, what
/// each node was last seen as, and what was ever unlinked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkSet {
    pub links: Vec<Link>,
    pub info: BTreeMap<Node, Info>,
    /// Every capture dir ever observed for a snapshot (each `Observed`
    /// `record` of a `Change(Snapshot)`), not only the latest: a review
    /// and a verification of one snapshot each keep their own.
    pub captures: BTreeMap<Node, BTreeSet<PathBuf>>,
    /// Every triple an `Unlinked` removed (it may have been linked again
    /// by hand since).
    pub unlinked: BTreeSet<Triple>,
    /// Every link id ever taken, unlinked ones included.
    pub ids: BTreeSet<LinkId>,
}

impl LinkSet {
    /// Folds one event in. Skipped: a `Linked` with a pair its `rel` does
    /// not allow, with an id already taken, naming a change id without
    /// its kind's form ([`ChangeId::is_valid`]), or a `Judged` from a
    /// review or verification while it is judged on another change; an
    /// `Observed` of such a change id. An `Unlinked` of no live link does
    /// nothing.
    pub fn apply(&mut self, event: &LinkEvent) {
        match event {
            LinkEvent::Linked {
                id,
                from,
                to,
                rel,
                origin,
                at,
            } => {
                if !rel.allows(from, to)
                    || !from.is_valid()
                    || !to.is_valid()
                    || self.ids.contains(id)
                    || (*rel == Rel::Judged && self.judged_elsewhere(from, to))
                {
                    return;
                }
                self.ids.insert(id.clone());
                self.links.push(Link {
                    id: id.clone(),
                    from: from.clone(),
                    to: to.clone(),
                    rel: *rel,
                    origin: *origin,
                    at: *at,
                });
            }
            LinkEvent::Unlinked { id, .. } => {
                if let Some(at) = self.links.iter().position(|l| &l.id == id) {
                    let link = self.links.remove(at);
                    self.unlinked.insert((link.from, link.to, link.rel));
                }
            }
            LinkEvent::Observed { node, info, .. } => {
                if !node.is_valid() {
                    return;
                }
                if let (Node::Change(ChangeId::Snapshot(_)), Some(dir)) = (node, &info.record) {
                    self.captures
                        .entry(node.clone())
                        .or_default()
                        .insert(dir.clone());
                }
                self.info.entry(node.clone()).or_default().merge(info);
            }
        }
    }

    /// The change `record` (a review or verification) is judged on: its
    /// live `Judged` link's (a record has at most one).
    pub fn judged(&self, record: &Node) -> Option<&ChangeId> {
        self.links_from(record, Rel::Judged)
            .find_map(|l| change_of(&l.to))
    }

    /// Whether `record` is judged on a change other than `change`.
    fn judged_elsewhere(&self, record: &Node, change: &Node) -> bool {
        self.judged(record)
            .is_some_and(|judged| change_of(change) != Some(judged))
    }

    /// The live link `from → to` of `rel`.
    pub fn live(&self, from: &Node, to: &Node, rel: Rel) -> Option<&Link> {
        self.links
            .iter()
            .find(|l| l.rel == rel && &l.from == from && &l.to == to)
    }

    /// Whether `from → to` of `rel` was ever linked, unlinked or not.
    pub fn ever(&self, from: &Node, to: &Node, rel: Rel) -> bool {
        self.live(from, to, rel).is_some()
            || self.unlinked.contains(&(from.clone(), to.clone(), rel))
    }

    /// The live link with id `id`.
    pub fn link(&self, id: &LinkId) -> Option<&Link> {
        self.links.iter().find(|l| &l.id == id)
    }

    /// The live links of `rel` from `node`, in link order.
    pub fn links_from<'a>(&'a self, node: &Node, rel: Rel) -> impl Iterator<Item = &'a Link> {
        self.links
            .iter()
            .filter(move |l| l.rel == rel && &l.from == node)
    }

    /// The live links of `rel` to `node`, in link order.
    pub fn links_to<'a>(&'a self, node: &Node, rel: Rel) -> impl Iterator<Item = &'a Link> {
        self.links
            .iter()
            .filter(move |l| l.rel == rel && &l.to == node)
    }

    /// What `node` was last seen as (empty when never observed).
    pub fn info_of(&self, node: &Node) -> Info {
        self.info.get(node).cloned().unwrap_or_default()
    }

    /// Every node a live link joins or an `Observed` names.
    pub fn nodes(&self) -> BTreeSet<&Node> {
        self.links
            .iter()
            .flat_map(|l| [&l.from, &l.to])
            .chain(self.info.keys())
            .collect()
    }
}

/// Folds `events` in order.
pub fn fold(events: &[LinkEvent]) -> LinkSet {
    let mut set = LinkSet::default();
    for event in events {
        set.apply(event);
    }
    set
}

// ---------------------------------------------------------------- store

/// `<data dir>/noa`: where Noa's session state, the runs logs, the reviews
/// and the verifications live.
pub fn default_data() -> Option<PathBuf> {
    let state = noa_config::session_state_path()?;
    Some(state.parent()?.to_path_buf())
}

/// Where the stores live: `<data dir>/noa/links`.
pub fn default_root() -> Option<PathBuf> {
    Some(default_data()?.join("links"))
}

/// The directory name of `repo`'s files under a root:
/// `<dir name>-<FNV-1a 64 of the path, hex>` (as the tasks widget's
/// `store::repo_key`).
pub fn repo_key(repo: &Path) -> String {
    let repo: PathBuf = repo.components().collect();
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_owned());
    format!("{name}-{:016x}", fnv1a(repo.as_os_str().as_bytes()))
}

/// The store of the repository `repo` is in, from any of its worktrees:
/// `<root>/<repo_key(main worktree)>/links.jsonl`.
pub fn path(root: &Path, repo: &Path) -> Result<PathBuf, String> {
    Ok(root.join(repo_key(&main_worktree(repo)?)).join(LINKS_FILE))
}

/// Holds the store's lock (`links.jsonl.lock` beside `log`) until dropped.
pub fn lock(log: &Path) -> io::Result<fs::File> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut name = log.as_os_str().to_owned();
    name.push(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW)
        .open(PathBuf::from(name))?;
    // SAFETY: flock on a descriptor this function owns.
    while unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    Ok(file)
}

/// A link id not in `taken`: `lnk-<now>`, then `lnk-<now>-2`, `-3` …
pub fn new_id(now: Millis, taken: &BTreeSet<LinkId>) -> LinkId {
    (1u32..)
        .map(|n| match n {
            1 => LinkId(format!("lnk-{now}")),
            n => LinkId(format!("lnk-{now}-{n}")),
        })
        .find(|id| !taken.contains(id))
        .expect("an unbounded range has a free id")
}

/// One change [`append`] is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Link {
        from: Node,
        to: Node,
        rel: Rel,
        origin: Origin,
    },
    Unlink(LinkId),
    Observe {
        node: Node,
        info: Info,
    },
}

/// What [`append`] did with one [`Edit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Linked under this id.
    Linked(LinkId),
    Unlinked,
    Observed,
    /// Nothing to write: an automatic link whose triple was ever linked,
    /// an unlink of no live link, an observation the node already has.
    Skipped,
    /// Refused, with the reason: a pair the relation does not allow, a
    /// change id without its kind's form, a manual link that is already
    /// live ([`ALREADY_LINKED`]), a `Judged` from a record judged on
    /// another change ([`JUDGED_ELSEWHERE`]).
    Refused(String),
}

/// Under the store's lock: replays it, decides each edit against it (and
/// the edits before it), and appends what is left in one write.
///
/// - `Link` with a pair its `rel` does not allow, or naming a change id
///   without its kind's form: refused.
/// - `Link` `Auto`: skipped when its triple was **ever** linked, so a
///   person's unlink is not undone by the next automatic recording.
/// - `Link` `Manual`: refused as [`ALREADY_LINKED`] while one is live.
/// - `Link` `Judged` from a review or verification with a live `Judged`
///   to another change: refused as [`JUDGED_ELSEWHERE`] (a result is about
///   one exact change).
/// - `Unlink` of no live link: skipped.
/// - `Observe` of a change id without its kind's form: refused; one that
///   changes no field of the node's folded info: skipped.
pub fn append(log: &Path, edits: &[Edit], now: Millis) -> io::Result<Vec<Outcome>> {
    append_with(log, now, |_| Ok(edits.to_vec()))
}

/// [`append`], with the edits made under the lock from the store as it is
/// then: a choice that depends on what is recorded (a hand-off's source)
/// is made where no other writer can change it before it is written. An
/// `Err` from `edits` writes nothing.
fn append_with(
    log: &Path,
    now: Millis,
    edits: impl FnOnce(&LinkSet) -> io::Result<Vec<Edit>>,
) -> io::Result<Vec<Outcome>> {
    let _lock = lock(log)?;
    let mut set = replay(log)?;
    let edits = edits(&set)?;
    let mut events = Vec::new();
    let mut outcomes = Vec::with_capacity(edits.len());
    for edit in &edits {
        let (outcome, event) = decide(&set, edit, now);
        if let Some(event) = event {
            set.apply(&event);
            events.push(event);
        }
        outcomes.push(outcome);
    }
    if !events.is_empty() {
        write_lines(log, &events)?;
    }
    Ok(outcomes)
}

fn decide(set: &LinkSet, edit: &Edit, now: Millis) -> (Outcome, Option<LinkEvent>) {
    match edit {
        Edit::Link {
            from,
            to,
            rel,
            origin,
        } => {
            if !rel.allows(from, to) {
                return (
                    Outcome::Refused(format!(
                        "{} から {} へ {rel:?} のリンクは作れません",
                        kind(from),
                        kind(to)
                    )),
                    None,
                );
            }
            if let Some(why) = invalid_change(&[from, to]) {
                return (Outcome::Refused(why), None);
            }
            match origin {
                Origin::Auto if set.ever(from, to, *rel) => return (Outcome::Skipped, None),
                Origin::Manual if set.live(from, to, *rel).is_some() => {
                    return (Outcome::Refused(ALREADY_LINKED.to_owned()), None);
                }
                _ => {}
            }
            if *rel == Rel::Judged && set.judged_elsewhere(from, to) {
                return (Outcome::Refused(JUDGED_ELSEWHERE.to_owned()), None);
            }
            let id = new_id(now, &set.ids);
            (
                Outcome::Linked(id.clone()),
                Some(LinkEvent::Linked {
                    id,
                    from: from.clone(),
                    to: to.clone(),
                    rel: *rel,
                    origin: *origin,
                    at: now,
                }),
            )
        }
        Edit::Unlink(id) => match set.link(id) {
            Some(_) => (
                Outcome::Unlinked,
                Some(LinkEvent::Unlinked {
                    id: id.clone(),
                    at: now,
                }),
            ),
            None => (Outcome::Skipped, None),
        },
        Edit::Observe { node, info } => {
            if let Some(why) = invalid_change(&[node]) {
                return (Outcome::Refused(why), None);
            }
            let mut folded = set.info_of(node);
            if folded.merge(info) {
                (
                    Outcome::Observed,
                    Some(LinkEvent::Observed {
                        node: node.clone(),
                        info: info.clone(),
                        at: now,
                    }),
                )
            } else {
                (Outcome::Skipped, None)
            }
        }
    }
}

/// The change id among `nodes` without its kind's form, and why an edit
/// naming it is refused.
fn invalid_change(nodes: &[&Node]) -> Option<String> {
    nodes
        .iter()
        .filter_map(|n| change_of(n))
        .find(|c| !c.is_valid())
        .map(|c| format!("{:?} は版の ID の形ではありません", c.as_str()))
}

fn kind(node: &Node) -> &'static str {
    match node {
        Node::Task(_) => "タスク",
        Node::Run(_) => "実行",
        Node::Change(_) => "版",
        Node::Review(_) => "レビュー",
        Node::Verify(_) => "検証",
    }
}

/// Appends `events`, one line each, in one write. A torn last line (a
/// writer died mid-write) is ended first, so these are lines of their own.
fn write_lines(log: &Path, events: &[LinkEvent]) -> io::Result<()> {
    use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _};
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(log)?;
    let len = file.metadata()?.len();
    let mut last = [0u8; 1];
    let torn = len > 0 && {
        file.read_exact_at(&mut last, len - 1)?;
        last[0] != b'\n'
    };
    let mut lines = if torn { b"\n".to_vec() } else { Vec::new() };
    for event in events {
        serde_json::to_writer(&mut lines, event).map_err(io::Error::other)?;
        lines.push(b'\n');
    }
    // One write of every line, so a reader never sees half of one unless
    // the process dies in between.
    (&file).write_all(&lines)?;
    file.sync_data()
}

/// The store's events; a line that does not parse (a torn last line, a
/// hand edit) is skipped. A missing store is empty.
pub fn read_events(log: &Path) -> io::Result<Vec<LinkEvent>> {
    let text = match fs::read(log) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(text
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect())
}

/// The store folded, read without the lock.
pub fn replay(log: &Path) -> io::Result<LinkSet> {
    Ok(fold(&read_events(log)?))
}

/// Now, in [`Millis`].
pub fn now_millis() -> Millis {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as Millis)
}

// ------------------------------------------------------------------ git

/// Settings that shape what git prints, pinned as verify's capture pins
/// them, so a person's config cannot change a snapshot id. The settings
/// an option overrides are pinned by the option in [`SNAPSHOT_DIFF`]
/// (`diff.orderFile`, `diff.submodule`, `diff.algorithm`,
/// `diff.indentHeuristic`, `diff.interHunkContext`, `diff.context`,
/// `diff.renames`, `diff.external`, the prefixes); these have none.
/// `status.showStash` would make a clean tree's status print a
/// `# stash <n>` header.
const PINNED: [(&str, &str); 10] = [
    ("core.quotePath", "false"),
    ("core.bigFileThreshold", "512m"),
    ("diff.noprefix", "false"),
    ("diff.mnemonicPrefix", "false"),
    ("diff.relative", "false"),
    ("diff.ignoreSubmodules", "none"),
    ("diff.suppressBlankEmpty", "false"),
    ("core.fsmonitor", "false"),
    ("color.ui", "false"),
    ("status.showStash", "false"),
];

/// `git diff` of a snapshot (work tree against `HEAD`, `.noa/` left out):
/// a binary patch with full object ids, no external diff or text
/// conversion, no color, no renames, and every option that shapes it at
/// git's default whatever the settings say: three lines of context, hunks
/// joined only where they touch, git's own file order (no order file),
/// the Myers algorithm with the indent heuristic, the short submodule
/// lines, no submodule ignored, `a/` and `b/` prefixes, the whole tree's
/// paths.
const SNAPSHOT_DIFF: [&str; 19] = [
    "diff",
    "--binary",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--ignore-submodules=none",
    "--submodule=short",
    "--no-renames",
    "-U3",
    "--inter-hunk-context=0",
    "-O/dev/null",
    "--diff-algorithm=myers",
    "--indent-heuristic",
    "--no-relative",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "HEAD",
    "--",
    NOT_NOA,
];

/// Clears every `GIT_*` variable `cmd` would inherit, so git finds the
/// repository from its directory alone.
fn without_git_env(cmd: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            cmd.env_remove(name);
        }
    }
}

/// 64-bit FNV-1a.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = Fnv::new();
    hash.update(bytes);
    hash.0
}

/// 64-bit FNV-1a, fed in parts.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn update(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// `git -C dir`, without the person's `GIT_*` variables, with [`PINNED`]
/// and no optional locks (a status never writes the index under an agent
/// at work in the tree).
fn git_cmd(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    without_git_env(&mut cmd);
    cmd.arg("--no-optional-locks");
    for (key, value) in PINNED {
        cmd.arg("-c").arg(format!("{key}={value}"));
    }
    cmd.arg("-C").arg(dir).stdin(Stdio::null());
    cmd
}

/// `git -C dir <args>`'s stdout, or why it failed.
fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    run_git(git_cmd(dir), args)
}

/// `cmd <args>`'s stdout, or why it failed.
fn run_git(mut cmd: Command, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = cmd
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("git を起動できません: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// `cmd <args>`'s stdout, read up to `cap` bytes: `Ok(None)` past that
/// (git is stopped).
fn git_capped(mut cmd: Command, args: &[&str], cap: u64) -> Result<Option<Vec<u8>>, String> {
    let mut child = cmd
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git を起動できません: {e}"))?;
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let errors = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stderr.read_to_end(&mut text);
        text
    });
    let mut out = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("stdout is piped")
        .take(cap + 1)
        .read_to_end(&mut out);
    if read.is_err() || out.len() as u64 > cap {
        let _ = child.kill();
        let _ = child.wait();
        let _ = errors.join();
        return match read {
            Err(e) => Err(format!("git の出力を読めません: {e}")),
            Ok(_) => Ok(None),
        };
    }
    let status = child.wait().map_err(|e| format!("git を待てません: {e}"))?;
    let errors = errors.join().unwrap_or_default();
    if status.success() {
        Ok(Some(out))
    } else {
        Err(String::from_utf8_lossy(&errors).trim().to_owned())
    }
}

/// A path git printed, its line end dropped.
fn printed_path(mut out: Vec<u8>) -> PathBuf {
    while out.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
        out.pop();
    }
    PathBuf::from(OsString::from_vec(out))
}

/// Text git printed, its line end dropped.
fn printed_text(out: Vec<u8>) -> String {
    String::from_utf8_lossy(&out).trim_end().to_owned()
}

/// The main work tree of the repository `repo` is in: the first entry of
/// `git worktree list --porcelain` (from a linked worktree
/// `/src/app-wt/feature` of `/src/app`, it is `/src/app`).
pub fn main_worktree(repo: &Path) -> Result<PathBuf, String> {
    let out = git(repo, &["worktree", "list", "--porcelain", "-z"])
        .map_err(|why| format!("{} の worktree を一覧できません: {why}", repo.display()))?;
    out.split(|b| *b == 0)
        .find_map(|field| field.strip_prefix(b"worktree "))
        .map(|path| PathBuf::from(OsString::from_vec(path.to_vec())))
        .ok_or_else(|| "git worktree list が worktree を返しませんでした".to_owned())
}

// -------------------------------------------------------- target version

/// The `.noa/` folder (the widgets' own files) left out of every listing
/// and diff of a snapshot.
const NOT_NOA: &str = ":(exclude).noa";

/// The change the work tree holding `workdir` is at now:
///
/// - `Err` on an unborn `HEAD`, or when git fails;
/// - `Commit(HEAD)` when nothing outside `.noa/` differs from `HEAD`;
/// - `Err` when a submodule has changes in its own work tree, or the tree
///   holds an untracked repository: the diff names only a commit for
///   those, not what is inside, so two different states would get one id;
/// - else `Snapshot(snap-<HEAD>-<16 hex>)`, the FNV-1a 64 of
///   `noa-snap-v1\0`, `HEAD`, `\0`, the binary diff of the work tree
///   against `HEAD` ([`SNAPSHOT_DIFF`]), then each untracked entry, sorted
///   by path: its path, `\0`, its kind (`f` a file, `x` a file with its
///   owner's execute bit, `l` a link), its length (u64 LE) and its bytes
///   (a link: its target, not followed). Over [`SNAPSHOT_MAX_BYTES`] of
///   diff and untracked bytes: `Err(TOO_LARGE)`.
///
/// `HEAD` is read again once the state is read: when it moved meanwhile,
/// `Err` (the id would name one commit over a state on another). The
/// tree's index is never written: the state is read through a copy of it
/// ([`IndexCopy`]).
///
/// Equal states get equal ids, whichever widget names them.
pub fn snapshot_id(workdir: &Path) -> Result<ChangeId, String> {
    snapshot_id_with(workdir, || {})
}

/// [`snapshot_id`], running `between` once `HEAD` is first read (a test
/// moves `HEAD` there).
fn snapshot_id_with(workdir: &Path, between: impl FnOnce()) -> Result<ChangeId, String> {
    let top = git(workdir, &["rev-parse", "--show-toplevel"])
        .map(printed_path)
        .map_err(|why| {
            format!(
                "{} は Git の作業ツリーではありません: {why}",
                workdir.display()
            )
        })?;
    let head = read_head(&top)?;
    between();
    let id = state_id(&top, &head)?;
    if read_head(&top)? != head {
        return Err(HEAD_MOVED.to_owned());
    }
    Ok(id)
}

/// The commit `HEAD` of the work tree `top` is at.
fn read_head(top: &Path) -> Result<String, String> {
    git(top, &["rev-parse", "--verify", "-q", "HEAD^{commit}"])
        .map(printed_text)
        .map_err(|_| format!("{} にはまだコミットがありません", top.display()))
}

/// A private copy of the index of the work tree `top`, removed with its
/// folder when dropped. A snapshot's git calls read and write only it
/// ([`IndexCopy::git`]): `git diff` writes the stat info it refreshes back
/// to the index it read, `--no-optional-locks` or not, and the tree's own
/// index belongs to the agent at work there.
struct IndexCopy {
    dir: PathBuf,
    index: PathBuf,
}

impl IndexCopy {
    fn of(top: &Path) -> Result<IndexCopy, String> {
        use std::os::unix::fs::DirBuilderExt as _;
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let own = top.join(printed_path(git(
            top,
            &["rev-parse", "--git-path", "index"],
        )?));
        let dir = std::env::temp_dir().join(format!(
            "noa-links-index-{}-{}-{}",
            std::process::id(),
            now_millis(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("{} を作れません: {e}", dir.display()))?;
        let copy = IndexCopy {
            index: dir.join("index"),
            dir,
        };
        match fs::copy(&own, &copy.index) {
            // No index yet: git reads none as an empty one, here as there.
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                Err(format!("{} を読めません: {e}", own.display()))
            }
            _ => Ok(copy),
        }
    }

    /// [`git_cmd`] on this copy; `core.splitIndex` off, so a write of the
    /// copy never writes a shared index into the repository.
    fn git(&self, top: &Path) -> Command {
        let mut cmd = git_cmd(top);
        cmd.args(["-c", "core.splitIndex=false"])
            .env("GIT_INDEX_FILE", &self.index);
        cmd
    }
}

impl Drop for IndexCopy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The id of the state of the work tree `top` on `head` ([`snapshot_id`]).
/// Its git calls read a copy of the index ([`IndexCopy`]), never write
/// the tree's own.
fn state_id(top: &Path, head: &str) -> Result<ChangeId, String> {
    let index = IndexCopy::of(top)?;
    let status = run_git(
        index.git(top),
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
            "--",
            NOT_NOA,
        ],
    )?;
    if !dirty(&status) {
        return Ok(ChangeId::Commit(head.to_owned()));
    }
    if nested_change(&status) {
        return Err(NESTED.to_owned());
    }
    let mut hash = Fnv::new();
    hash.update(b"noa-snap-v1\0");
    hash.update(head.as_bytes());
    hash.update(b"\0");
    let diff = git_capped(index.git(top), &SNAPSHOT_DIFF, SNAPSHOT_MAX_BYTES)?
        .ok_or_else(|| TOO_LARGE.to_owned())?;
    hash.update(&diff);
    let mut budget = SNAPSHOT_MAX_BYTES - diff.len() as u64;
    let listing = run_git(
        index.git(top),
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            NOT_NOA,
        ],
    )?;
    let mut untracked: Vec<&[u8]> = listing
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .collect();
    untracked.sort_unstable();
    for name in untracked {
        let path = top.join(OsStr::from_bytes(name));
        let (kind, bytes) = untracked_entry(&path, budget)?;
        budget -= bytes.len() as u64;
        hash.update(name);
        hash.update(b"\0");
        hash.update(&[kind]);
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    Ok(ChangeId::Snapshot(format!("snap-{head}-{:016x}", hash.0)))
}

/// Whether `git status --porcelain=v2 -z` names an entry: a field that is
/// not a `#` header line (`# stash <n>`, `# branch.…`), which a setting
/// can add to a clean tree's status. Every entry starts with its kind
/// (`1`, `2`, `u`, `?`, `!`); a rename's original path, a field of its
/// own, follows an entry.
fn dirty(status: &[u8]) -> bool {
    status
        .split(|b| *b == 0)
        .any(|field| !field.is_empty() && !field.starts_with(b"#"))
}

/// Whether `git status --porcelain=v2 -z` names a submodule with changes
/// in its own work tree (its `<sub>` field `S?M?` or `S??U`), or an
/// untracked repository (`? dir/`: with every untracked file listed, a
/// directory is listed only when it holds a repository of its own).
fn nested_change(status: &[u8]) -> bool {
    let mut fields = status.split(|b| *b == 0);
    while let Some(entry) = fields.next() {
        let mut words = entry.splitn(4, |b| *b == b' ');
        let (kind, _xy, sub) = (words.next(), words.next(), words.next());
        match kind {
            Some(b"?") if entry.ends_with(b"/") => return true,
            Some(b"1" | b"2" | b"u") => {
                if kind == Some(b"2") {
                    // A rename's original path is a field of its own.
                    fields.next();
                }
                if let Some(&[b'S', _, m, u]) = sub
                    && (m == b'M' || u == b'U')
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// What an untracked entry adds to a snapshot: its kind (`f` a file, `x`
/// a file with its owner's execute bit, as git tells 100755 from 100644;
/// `l` a link) and its bytes (a file's, a link's target, not followed).
/// `Err(NESTED)` for a directory (a repository of its own), `Err(TOO_LARGE)`
/// past `budget`.
fn untracked_entry(path: &Path, budget: u64) -> Result<(u8, Vec<u8>), String> {
    use std::os::unix::fs::PermissionsExt as _;
    let unreadable = |e: io::Error| format!("{} を読めません: {e}", path.display());
    let meta = fs::symlink_metadata(path).map_err(unreadable)?;
    let (kind, bytes) = if meta.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(unreadable)?;
        (b'l', target.into_os_string().into_vec())
    } else if meta.is_file() {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .and_then(|f| f.take(budget + 1).read_to_end(&mut bytes))
            .map_err(unreadable)?;
        let executable = meta.permissions().mode() & 0o100 != 0;
        (if executable { b'x' } else { b'f' }, bytes)
    } else {
        return Err(NESTED.to_owned());
    };
    if bytes.len() as u64 > budget {
        return Err(TOO_LARGE.to_owned());
    }
    Ok((kind, bytes))
}

/// The change the work tree holding `workdir` is at now: [`snapshot_id`]
/// (a clean tree is its `HEAD` commit).
pub fn current_change(workdir: &Path) -> Result<ChangeId, String> {
    snapshot_id(workdir)
}

/// A task's current change, and the run it was taken from ([`task_current`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    /// The run whose work tree is at it, or whose last produced change it
    /// is.
    pub run: String,
    pub change: ChangeId,
}

/// The current change of `task`: [`current_change`] of the work tree of
/// its newest run (by start) whose work tree still exists; when none does,
/// the last change its newest run produced ([`last_produced`], by commit
/// topology in `repo`, the main worktree). With the run it came from,
/// under which [`related`] lists it. `None` with no run, when git cannot
/// read the work tree, or when the newest run produced nothing recorded.
/// Runs git: call it off the UI thread.
pub fn task_current(set: &LinkSet, task: &TaskKey, repo: &Path) -> Option<Current> {
    let me = Node::Task(task.clone());
    let ran = runs_by_start(set, &me);
    let newest = ran.last()?;
    for (run, link) in ran.iter().rev() {
        if let Some(workdir) = set.info.get(&link.to).and_then(|i| i.workdir.as_deref())
            && workdir.is_dir()
        {
            return current_change(workdir).ok().map(|change| Current {
                run: (*run).to_owned(),
                change,
            });
        }
    }
    let (run, link) = newest;
    Some(Current {
        run: (*run).to_owned(),
        change: last_produced(set, &link.to, repo)?,
    })
}

/// The last of the changes `run` produced, by commit topology in `repo`
/// (not by the order they were recorded in: a hand-off can record v2
/// before a later pass records v1):
///
/// 1. The tips: the commits the changes are or sit on (a snapshot's head)
///    that none of the others descends from (`git merge-base
///    --independent`). A commit git cannot find, or every commit when git
///    fails, counts as a tip (nothing places it under another).
/// 2. One tip: that commit. Several (a rebase, history that split): the
///    tip of the latest recorded change on any of them.
/// 3. Of that commit and the snapshots on it, the latest recorded (work
///    restored to the commit after a snapshot of it is recorded after the
///    snapshot).
///
/// `None` when the run produced nothing recorded.
fn last_produced(set: &LinkSet, run: &Node, repo: &Path) -> Option<ChangeId> {
    let produced: Vec<&ChangeId> = set
        .links_from(run, Rel::Produced)
        .filter_map(|l| change_of(&l.to))
        .collect();
    let mut heads: Vec<&str> = produced.iter().filter_map(|c| c.head()).collect();
    heads.sort_unstable();
    heads.dedup();
    let tips = tips(repo, &heads);
    let tip = produced
        .iter()
        .rev()
        .filter_map(|c| c.head())
        .find(|head| tips.contains(head))?;
    produced
        .iter()
        .rev()
        .find(|c| c.head() == Some(tip))
        .map(|c| (*c).clone())
}

/// Of the commits `heads` (distinct), those no other of them descends
/// from, in `repo`: `git merge-base --independent` over the ones git
/// finds, plus every one it does not; all of them when git fails.
fn tips<'a>(repo: &Path, heads: &[&'a str]) -> BTreeSet<&'a str> {
    let all = || heads.iter().copied().collect();
    if heads.len() < 2 {
        return all();
    }
    let Some(found) = existing_commits(repo, heads) else {
        return all();
    };
    let (known, unknown): (Vec<&str>, Vec<&str>) =
        heads.iter().copied().partition(|sha| found.contains(*sha));
    let mut tips: BTreeSet<&str> = unknown.into_iter().collect();
    if known.len() < 2 {
        tips.extend(known);
        return tips;
    }
    let mut args = vec!["merge-base", "--independent"];
    args.extend(&known);
    match git(repo, &args) {
        Ok(out) => {
            let printed = printed_text(out);
            let independent: BTreeSet<&str> = printed.lines().map(str::trim).collect();
            tips.extend(known.iter().filter(|sha| independent.contains(**sha)));
        }
        Err(_) => tips.extend(known),
    }
    tips
}

/// The live `Ran` links from `task`, each with its run id, oldest run
/// first: by the start its id names (`run-<millis>`), else by when it was
/// linked.
fn runs_by_start<'a>(set: &'a LinkSet, task: &'a Node) -> Vec<(&'a str, &'a Link)> {
    let mut ran: Vec<(&str, &Link)> = set
        .links_from(task, Rel::Ran)
        .filter_map(|l| match &l.to {
            Node::Run(run) => Some((run.as_str(), l)),
            _ => None,
        })
        .collect();
    ran.sort_by_key(|(run, l)| run_start(run).unwrap_or(l.at));
    ran
}

/// `run-<millis>[-n]` → millis.
fn run_start(run: &str) -> Option<Millis> {
    run.strip_prefix("run-")?.split('-').next()?.parse().ok()
}

fn change_of(node: &Node) -> Option<&ChangeId> {
    match node {
        Node::Change(change) => Some(change),
        _ => None,
    }
}

// -------------------------------------------------------------- presence

/// Whether the record a node points at can still be found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    Found,
    /// Gone; `what` names what it pointed to (its id and path, or the SHA).
    Missing {
        what: String,
    },
    /// Not known: git failed, nothing recorded to look at, a snapshot with
    /// no capture dir whose work tree is still there (whether that tree is
    /// still at it is not checked), a GitHub issue (not checked over the
    /// network).
    Unknown,
}

/// `<data>/reviews/<id>`, for an id of the `rev-<millis>[-n]` form only:
/// the path is rebuilt from the id, never taken from the store.
pub fn review_dir(data: &Path, id: &str) -> Option<PathBuf> {
    id_ok(id, "rev-").then(|| data.join("reviews").join(id))
}

/// `<data>/verifications/<id>`, for an id of the `ver-<millis>[-n]` form only.
pub fn verify_dir(data: &Path, id: &str) -> Option<PathBuf> {
    id_ok(id, "ver-").then(|| data.join("verifications").join(id))
}

/// `<prefix><digits>[-<digits>]`.
fn id_ok(id: &str, prefix: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let Some(rest) = id.strip_prefix(prefix) else {
        return false;
    };
    match rest.split_once('-') {
        Some((millis, n)) => digits(millis) && digits(n),
        None => digits(rest),
    }
}

/// A local task's id as Tasks takes one (`t-` and at least one more
/// character: `t-<millis>[-n]`, or a name given by hand such as
/// `t-fix-ui` or `t-review-v1..v2`), and never a path out of the tasks
/// folder: no `/` or NUL (so `..` is never a component of its own, and
/// the `t-` prefix rules out an id that is `.` or `..`).
fn local_task_id(id: &str) -> bool {
    id.len() > 2 && id.starts_with("t-") && !id.contains(['/', '\0'])
}

/// The presence of every node of `set` (`data`: `<data dir>/noa`; `repo`:
/// the main worktree, where commits and task files are looked up). One git
/// call for all the commits; run it off the UI thread.
pub fn presences(set: &LinkSet, data: &Path, repo: &Path) -> BTreeMap<Node, Presence> {
    check(set, set.nodes(), data, repo)
}

/// The presence of `node` (see [`presences`]).
pub fn presence(set: &LinkSet, node: &Node, data: &Path, repo: &Path) -> Presence {
    check(set, BTreeSet::from([node]), data, repo)
        .remove(node)
        .unwrap_or(Presence::Unknown)
}

fn check(
    set: &LinkSet,
    nodes: BTreeSet<&Node>,
    data: &Path,
    repo: &Path,
) -> BTreeMap<Node, Presence> {
    let shas: Vec<&str> = nodes
        .iter()
        .filter_map(|n| match n {
            Node::Change(ChangeId::Commit(sha)) if is_sha(sha) => Some(sha.as_str()),
            _ => None,
        })
        .collect();
    let commits = existing_commits(repo, &shas);
    nodes
        .into_iter()
        .map(|node| {
            let found = presence_of(set, node, data, repo, commits.as_ref());
            (node.clone(), found)
        })
        .collect()
}

fn presence_of(
    set: &LinkSet,
    node: &Node,
    data: &Path,
    repo: &Path,
    commits: Option<&BTreeSet<String>>,
) -> Presence {
    let info = set.info.get(node);
    let record = info.and_then(|i| i.record.clone());
    let missing = |id: &str, path: Option<&Path>| Presence::Missing {
        what: match path {
            Some(path) => format!("{id} {}", path.display()),
            None => id.to_owned(),
        },
    };
    let file = |id: &str, path: PathBuf| {
        if path.is_file() {
            Presence::Found
        } else {
            missing(id, Some(&path))
        }
    };
    match node {
        Node::Task(TaskKey::GitHub { .. }) => Presence::Unknown,
        Node::Task(TaskKey::Local(id)) => match record {
            Some(path) => file(id, path),
            None if local_task_id(id) => {
                file(id, repo.join(".noa").join("tasks").join(format!("{id}.md")))
            }
            None => missing(id, None),
        },
        Node::Task(TaskKey::Code { path, .. }) => {
            file(path, record.unwrap_or_else(|| repo.join(path)))
        }
        Node::Run(run) => match record {
            Some(log) => run_in_log(run, &log),
            None => Presence::Unknown,
        },
        Node::Change(ChangeId::Commit(sha)) => match commits {
            _ if !is_sha(sha) => missing(sha, None),
            Some(found) if found.contains(sha) => Presence::Found,
            Some(_) => missing(sha, None),
            None => Presence::Unknown,
        },
        // Found while any of its capture dirs is: a review's and a
        // verification's capture of one snapshot are each a record of it.
        // With none recorded, missing once every work tree it was taken
        // in (its own, its producing runs') is gone: nothing can show it.
        Node::Change(ChangeId::Snapshot(id)) => {
            let gone = |dirs: Vec<&PathBuf>| Presence::Missing {
                what: std::iter::once(id.clone())
                    .chain(dirs.iter().map(|dir| dir.display().to_string()))
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            match set.captures.get(node) {
                Some(dirs) if dirs.iter().any(|dir| dir.is_dir()) => Presence::Found,
                Some(dirs) => gone(dirs.iter().collect()),
                None => {
                    let trees: BTreeSet<&PathBuf> = std::iter::once(node)
                        .chain(set.links_to(node, Rel::Produced).map(|l| &l.from))
                        .filter_map(|n| set.info.get(n)?.workdir.as_ref())
                        .collect();
                    if trees.is_empty() || trees.iter().any(|dir| dir.is_dir()) {
                        Presence::Unknown
                    } else {
                        gone(trees.into_iter().collect())
                    }
                }
            }
        }
        Node::Review(id) => match review_dir(data, id) {
            Some(dir) => file(id, dir.join("events.jsonl")),
            None => missing(id, None),
        },
        Node::Verify(id) => match verify_dir(data, id) {
            Some(dir) => file(id, dir.join("events.jsonl")),
            None => missing(id, None),
        },
    }
}

/// Whether the runs log `log` has a `RunStarted` line for `run`.
fn run_in_log(run: &str, log: &Path) -> Presence {
    let text = match fs::read(log) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Presence::Missing {
                what: format!("{run} {}", log.display()),
            };
        }
        Err(_) => return Presence::Unknown,
    };
    let started = text
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .any(|event| event["event"] == "RunStarted" && event["run"] == run);
    if started {
        Presence::Found
    } else {
        Presence::Missing {
            what: format!("{run} {}", log.display()),
        }
    }
}

/// Which of `shas` name a commit in `repo` (`git cat-file --batch-check`);
/// `None` when git cannot say.
fn existing_commits(repo: &Path, shas: &[&str]) -> Option<BTreeSet<String>> {
    if shas.is_empty() {
        return Some(BTreeSet::new());
    }
    let mut child = git_cmd(repo)
        .args(["cat-file", "--batch-check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let input: Vec<u8> = shas
        .iter()
        .flat_map(|sha| format!("{sha}^{{commit}}\n").into_bytes())
        .collect();
    // Written on a thread: git answers each line as it reads it.
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child.wait_with_output().ok()?;
    let _ = writer.join();
    if !output.status.success() {
        return None;
    }
    let found = shas
        .iter()
        .zip(output.stdout.split(|b| *b == b'\n'))
        .filter(|(_, line)| line.split(|b| *b == b' ').nth(1) == Some(b"commit"))
        .map(|(sha, _)| (*sha).to_owned())
        .collect();
    Some(found)
}

// ---------------------------------------------------------------- related

/// What a task's 関連成果 section draws ([`related`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Related {
    /// Its runs, oldest first.
    pub runs: Vec<RunEntry>,
    /// Changes linked to the task itself (`Produced` task → commit, added
    /// by hand), in link order; and the current change when the run it
    /// was taken from is none of the task's.
    pub changes: Vec<ChangeEntry>,
    /// Results linked `About` the task or one of its runs whose judged
    /// change is under none of them (「対象の版なし」).
    pub loose: Vec<ResultEntry>,
}

/// A row's links are only the task's own: links with the task itself at
/// one end (`Ran` task → run, `Produced` task → change, `About` result →
/// task), which リンクを外す removes. A link at a run (`Produced` run →
/// change, `About` result → run) or a result's `Judged` is shared by every
/// task that lists it, so no task's row carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEntry {
    pub run: String,
    /// The `Ran` link.
    pub link: LinkId,
    pub info: Info,
    /// In the order first recorded.
    pub changes: Vec<ChangeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEntry {
    pub change: ChangeId,
    /// The task's own `Produced` link (task → change, in
    /// [`Related::changes`]). `None` under a run (its `Produced` link is
    /// the run's) and for the current change when it was never recorded.
    pub link: Option<LinkId>,
    /// The task's current change (現在): only the row under the run
    /// [`Current::run`] names (or in [`Related::changes`] when that is
    /// none of the task's runs). The same change recorded under another
    /// run keeps its row there, not marked.
    pub current: bool,
    /// The reviews and verifications that judged exactly this change, by
    /// time; empty: 「この版の結果はまだありません」.
    pub results: Vec<ResultEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultEntry {
    /// `Review(id)` or `Verify(id)`.
    pub node: Node,
    /// The result's `About` links to the task itself, in link order
    /// (リンクを外す removes these). Empty for a result listed only for
    /// its `Judged` link or for an `About` link to one of the task's
    /// runs: those are shared with every task that lists the change or
    /// runs the run, so the row cannot be removed from one task's view.
    pub links: Vec<LinkId>,
    pub judged: Option<ChangeId>,
    /// It judged a change that is not the current one (旧版の結果).
    pub stale: bool,
    pub info: Info,
}

/// The 関連成果 of `task`, given its current change ([`task_current`]):
/// its runs, each with the changes it produced and under each the results
/// that judged exactly that change; results `About` the task or one of
/// its runs that judged none of them go to `loose`. The current change is
/// marked under the run it was taken from (in `changes` when that is none
/// of the task's runs), added there when that run never recorded it, with
/// every result that judged it. Each row carries only the task's own
/// links ([`RunEntry`]). Pure.
pub fn related(set: &LinkSet, task: &TaskKey, current: Option<&Current>) -> Related {
    let me = Node::Task(task.clone());
    let now = current.map(|c| &c.change);
    let ran = runs_by_start(set, &me);
    let mine: BTreeSet<&Node> = ran.iter().map(|(_, l)| &l.to).chain([&me]).collect();
    // The results About this task or one of its runs, in the order first
    // linked, and each one's About links to the task itself (its row's).
    let mut about_order: Vec<&Node> = Vec::new();
    let mut own: BTreeMap<&Node, Vec<LinkId>> = BTreeMap::new();
    for link in set
        .links
        .iter()
        .filter(|l| l.rel == Rel::About && mine.contains(&l.to))
    {
        if !about_order.contains(&&link.from) {
            about_order.push(&link.from);
        }
        if link.to == me {
            own.entry(&link.from).or_default().push(link.id.clone());
        }
    }

    let mut shown: BTreeSet<&Node> = BTreeSet::new();
    let mut entry = |change: &ChangeId, link: Option<LinkId>| -> ChangeEntry {
        shown.extend(
            set.links_to(&Node::Change(change.clone()), Rel::Judged)
                .map(|l| &l.from),
        );
        ChangeEntry {
            change: change.clone(),
            link,
            current: false,
            results: results_for(set, change, now, &own),
        }
    };
    let mut runs = Vec::new();
    for (run, link) in &ran {
        let changes = set
            .links_from(&link.to, Rel::Produced)
            .filter_map(|l| change_of(&l.to))
            .map(|change| entry(change, None))
            .collect();
        runs.push(RunEntry {
            run: (*run).to_owned(),
            link: link.id.clone(),
            info: set.info_of(&link.to),
            changes,
        });
    }
    let mut changes: Vec<ChangeEntry> = set
        .links_from(&me, Rel::Produced)
        .filter_map(|l| Some(entry(change_of(&l.to)?, Some(l.id.clone()))))
        .collect();
    if let Some(current) = current {
        let rows = match runs.iter_mut().find(|r| r.run == current.run) {
            Some(run) => &mut run.changes,
            None => &mut changes,
        };
        match rows.iter_mut().find(|c| c.change == current.change) {
            Some(row) => row.current = true,
            None => rows.push(ChangeEntry {
                current: true,
                ..entry(&current.change, None)
            }),
        }
    }

    let loose: Vec<ResultEntry> = about_order
        .into_iter()
        .filter(|node| !shown.contains(node))
        .map(|node| {
            let judged = set.judged(node).cloned();
            ResultEntry {
                node: node.clone(),
                links: own.get(node).cloned().unwrap_or_default(),
                stale: stale(judged.as_ref(), now),
                judged,
                info: set.info_of(node),
            }
        })
        .collect();
    Related {
        runs,
        changes,
        loose,
    }
}

/// The results that judged exactly `change`, by time, each with its
/// `About` links to the task (`own`).
fn results_for(
    set: &LinkSet,
    change: &ChangeId,
    current: Option<&ChangeId>,
    own: &BTreeMap<&Node, Vec<LinkId>>,
) -> Vec<ResultEntry> {
    let node = Node::Change(change.clone());
    let mut judged: Vec<&Link> = set.links_to(&node, Rel::Judged).collect();
    judged.sort_by_key(|l| l.at);
    judged
        .into_iter()
        .map(|l| ResultEntry {
            node: l.from.clone(),
            links: own.get(&l.from).cloned().unwrap_or_default(),
            judged: Some(change.clone()),
            stale: stale(Some(change), current),
            info: set.info_of(&l.from),
        })
        .collect()
}

/// A result is stale when it judged a change and the task is at another.
fn stale(judged: Option<&ChangeId>, current: Option<&ChangeId>) -> bool {
    matches!((judged, current), (Some(j), Some(c)) if j != c)
}

// -------------------------------------------------------------- backlinks

/// Where a review or verification came from ([`backlinks`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackLinks {
    /// `About` links to tasks, in link order.
    pub tasks: Vec<BackTask>,
    /// `About` links to runs, in link order.
    pub runs: Vec<BackRun>,
    /// Its `Judged` link: the change it judged (at most one).
    pub changes: Vec<BackChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackTask {
    pub task: TaskKey,
    pub link: LinkId,
    pub info: Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackRun {
    pub run: String,
    pub link: LinkId,
    pub info: Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackChange {
    pub change: ChangeId,
    pub link: LinkId,
    pub info: Info,
    /// The tasks whose 関連成果 list this change (版を開く opens the first;
    /// none: it copies the id).
    pub tasks: Vec<TaskKey>,
}

/// The tasks, runs and change `node` (a review or verification) links to.
/// Pure.
pub fn backlinks(set: &LinkSet, node: &Node) -> BackLinks {
    let mut back = BackLinks::default();
    for link in set.links.iter().filter(|l| &l.from == node) {
        match (link.rel, &link.to) {
            (Rel::About, Node::Task(task)) => back.tasks.push(BackTask {
                task: task.clone(),
                link: link.id.clone(),
                info: set.info_of(&link.to),
            }),
            (Rel::About, Node::Run(run)) => back.runs.push(BackRun {
                run: run.clone(),
                link: link.id.clone(),
                info: set.info_of(&link.to),
            }),
            (Rel::Judged, Node::Change(change)) => back.changes.push(BackChange {
                change: change.clone(),
                link: link.id.clone(),
                info: set.info_of(&link.to),
                tasks: tasks_of_change(set, &link.to),
            }),
            _ => {}
        }
    }
    back
}

/// The tasks that produced `change` themselves or through one of their runs.
fn tasks_of_change(set: &LinkSet, change: &Node) -> Vec<TaskKey> {
    let mut tasks = Vec::new();
    for link in set.links_to(change, Rel::Produced) {
        match &link.from {
            Node::Task(task) => push_new(&mut tasks, task.clone()),
            run @ Node::Run(_) => {
                for task in tasks_of_run(set, run) {
                    push_new(&mut tasks, task);
                }
            }
            _ => {}
        }
    }
    tasks
}

fn tasks_of_run(set: &LinkSet, run: &Node) -> Vec<TaskKey> {
    let mut tasks = Vec::new();
    for link in set.links_to(run, Rel::Ran) {
        if let Node::Task(task) = &link.from {
            push_new(&mut tasks, task.clone());
        }
    }
    tasks
}

fn push_new<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
}

// --------------------------------------------------------------- hand-off

/// Where a new review or verification came from ([`source_of`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Source {
    /// Runs it is `About`.
    pub runs: Vec<String>,
    /// Tasks it is `About` (each `Ran` link to those runs).
    pub tasks: Vec<TaskKey>,
    /// Rule 2: the run whose work tree it is, to record as having produced
    /// the change.
    pub produced_by: Option<String>,
}

/// The source of a review or verification of `change` in work tree
/// `workdir`, which is at `here` now ([`current_change`]; `None` when git
/// cannot say), exactly by these rules (no time window, no branch names):
///
/// 1. Some run has a `Produced` link to the change: those runs, and the
///    task of each `Ran` link to them.
/// 2. Else, exactly one run's observed info has this `workdir` and a
///    branch (a run in a worktree of its own), and the change is that work
///    tree's own: `here`, or the commit `here` sits on, and not the commit
///    the run started on (its observed `head`: a run that has not moved
///    off it did not make it). Then that run and its tasks, and
///    `produced_by` it. With no change (a Files review), the work tree
///    alone: that run and its tasks.
/// 3. Else none (the main checkout shared by runs made there, a commit no
///    run is known to have produced, a commit the work tree is not at).
///
/// A run's recorded work tree is `workdir` as written or as resolved
/// (`/tmp` and `/private/tmp`), which looks at the filesystem; under the
/// store's lock, [`record_handoff`] resolves the paths before taking it.
pub fn source_of(
    set: &LinkSet,
    change: Option<&ChangeId>,
    workdir: &Path,
    here: Option<&ChangeId>,
) -> Source {
    source_in(set, change, here, |w| same_dir(w, workdir))
}

/// [`source_of`], with `is_workdir` telling whether a run's recorded work
/// tree is the one handed off.
fn source_in(
    set: &LinkSet,
    change: Option<&ChangeId>,
    here: Option<&ChangeId>,
    is_workdir: impl Fn(&Path) -> bool,
) -> Source {
    if let Some(change) = change {
        let node = Node::Change(change.clone());
        let mut runs = Vec::new();
        for link in set.links_to(&node, Rel::Produced) {
            if let Node::Run(run) = &link.from {
                push_new(&mut runs, run.clone());
            }
        }
        if !runs.is_empty() {
            return Source {
                tasks: tasks_of_runs(set, &runs),
                runs,
                produced_by: None,
            };
        }
    }
    let owners: Vec<&String> = set
        .info
        .iter()
        .filter_map(|(node, info)| match node {
            Node::Run(run)
                if info.branch.is_some() && info.workdir.as_deref().is_some_and(&is_workdir) =>
            {
                Some(run)
            }
            _ => None,
        })
        .collect();
    match owners.as_slice() {
        [run] if change.is_none_or(|c| made_here(set, run, c, here)) => {
            let runs = vec![(*run).clone()];
            Source {
                tasks: tasks_of_runs(set, &runs),
                produced_by: change.map(|_| (*run).clone()),
                runs,
            }
        }
        _ => Source::default(),
    }
}

/// Rule 2's test of `change` for `run`, whose work tree is at `here`: it
/// is `here` or the commit `here` sits on, and not the commit `run`
/// started on.
fn made_here(set: &LinkSet, run: &str, change: &ChangeId, here: Option<&ChangeId>) -> bool {
    let Some(here) = here else {
        return false;
    };
    let at_tree = match change {
        ChangeId::Commit(sha) => here.head() == Some(sha.as_str()),
        ChangeId::Snapshot(_) => change == here,
    };
    let start = set
        .info
        .get(&Node::Run(run.to_owned()))
        .and_then(|i| i.head.as_deref());
    at_tree && !matches!(change, ChangeId::Commit(sha) if Some(sha.as_str()) == start)
}

fn tasks_of_runs(set: &LinkSet, runs: &[String]) -> Vec<TaskKey> {
    let mut tasks = Vec::new();
    for run in runs {
        for task in tasks_of_run(set, &Node::Run(run.clone())) {
            push_new(&mut tasks, task);
        }
    }
    tasks
}

/// One directory, as written or as resolved (`/tmp` and `/private/tmp`).
fn same_dir(a: &Path, b: &Path) -> bool {
    a.components().eq(b.components())
        || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// What [`record_handoff`] recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    /// Where the record came from ([`source_of`]).
    pub source: Source,
    /// Each edit the hand-off asked for, with what [`append`] did with it,
    /// in order: a `Refused` one was not recorded ([`Handoff::notice`]).
    pub outcomes: Vec<(Edit, Outcome)>,
}

impl Handoff {
    /// The notice naming each link (or observation) the store refused,
    /// with why (a change id without its form, …); `None` when everything
    /// asked for is recorded, now or before.
    pub fn notice(&self) -> Option<String> {
        let refused: Vec<String> = self
            .outcomes
            .iter()
            .filter_map(|(edit, outcome)| match outcome {
                Outcome::Refused(why) => Some(format!("{}: {why}", describe(edit))),
                _ => None,
            })
            .collect();
        (!refused.is_empty())
            .then(|| format!("リンクを記録できませんでした: {}", refused.join("、")))
    }
}

/// How a notice names an edit: `レビュー rev-1 → 版 abc1234 (Judged)`.
fn describe(edit: &Edit) -> String {
    match edit {
        Edit::Link { from, to, rel, .. } => {
            format!("{} → {} ({rel:?})", node_name(from), node_name(to))
        }
        Edit::Unlink(id) => format!("リンク {id} の削除"),
        Edit::Observe { node, .. } => format!("{} の情報", node_name(node)),
    }
}

/// `レビュー rev-1`, `版 abc1234`, `タスク t-1` …
fn node_name(node: &Node) -> String {
    let id = match node {
        Node::Task(task) => task.select(),
        Node::Change(change) => change.short(),
        Node::Run(id) | Node::Review(id) | Node::Verify(id) => id.clone(),
    };
    format!("{} {id}", kind(node))
}

/// Records a review or verification `record` just created on `change` (in
/// work tree `workdir`) in the store `log`: what it is (`info`), `Judged`
/// to the change, the snapshot's capture dir (`capture`), and the `About`
/// (and rule 2's `Produced`) links of [`source_of`]. Returns the source
/// and what the store did with each edit: a link it refused (a change id
/// without its form, …) is named by [`Handoff::notice`].
///
/// Git and the filesystem are read before the lock is taken: the work
/// tree's current change (rule 2; when it has no id, a snapshot `Err`,
/// the commit its `HEAD` is at, so an explicit commit that is the work
/// tree's own `HEAD` still finds its run), and which run work trees
/// recorded in the store resolve to `workdir`. The source is decided
/// under the lock, from the store as it is then and with no filesystem
/// lookup, so two hand-offs at once record what one after the other would
/// (one producer of a change, not one each); a run recorded meanwhile is
/// compared by its work tree as written. A record already judged on
/// another change is refused, `Err` of kind `AlreadyExists`
/// ([`JUDGED_ELSEWHERE`]), and nothing is written.
pub fn record_handoff(
    log: &Path,
    record: Node,
    change: Option<ChangeId>,
    workdir: &Path,
    info: Info,
    capture: Option<&Path>,
) -> io::Result<Handoff> {
    handoff(log, record, change, workdir, info, capture, || {})
}

/// [`record_handoff`], running `before_lock` once git and the filesystem
/// are read and before the lock is taken (a test records another hand-off
/// there, or removes a path).
fn handoff(
    log: &Path,
    record: Node,
    change: Option<ChangeId>,
    workdir: &Path,
    info: Info,
    capture: Option<&Path>,
    before_lock: impl FnOnce(),
) -> io::Result<Handoff> {
    let here = change.as_ref().and_then(|_| work_tree_at(workdir));
    let aliases = aliases_of(log, workdir);
    before_lock();
    let is_workdir = |w: &Path| w.components().eq(workdir.components()) || aliases.contains(w);
    let mut source = Source::default();
    let mut asked = Vec::new();
    let outcomes = append_with(log, now_millis(), |set| {
        if let Some(change) = &change
            && set.judged_elsewhere(&record, &Node::Change(change.clone()))
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                JUDGED_ELSEWHERE,
            ));
        }
        source = source_in(set, change.as_ref(), here.as_ref(), is_workdir);
        asked = handoff_edits(&record, change.as_ref(), workdir, info, capture, &source);
        Ok(asked.clone())
    })?;
    Ok(Handoff {
        source,
        outcomes: asked.into_iter().zip(outcomes).collect(),
    })
}

/// What rule 2 compares a hand-off's change with: the work tree's current
/// change, or, when that has no id (a snapshot `Err`: too large, a nested
/// repository, `HEAD` moved), the commit its `HEAD` is at. `None` when git
/// cannot read `HEAD`.
fn work_tree_at(workdir: &Path) -> Option<ChangeId> {
    current_change(workdir)
        .or_else(|_| read_head(workdir).map(ChangeId::Commit))
        .ok()
}

/// The run work trees recorded in the store `log` (read without its lock)
/// that resolve to the directory `workdir` does but are written another
/// way (`/tmp` and `/private/tmp`).
fn aliases_of(log: &Path, workdir: &Path) -> BTreeSet<PathBuf> {
    let (Ok(resolved), Ok(set)) = (fs::canonicalize(workdir), replay(log)) else {
        return BTreeSet::new();
    };
    set.info
        .iter()
        .filter_map(|(node, info)| match node {
            Node::Run(_) => info.workdir.as_ref(),
            _ => None,
        })
        .filter(|w| fs::canonicalize(w).is_ok_and(|w| w == resolved))
        .cloned()
        .collect()
}

/// What a hand-off writes, given its source.
fn handoff_edits(
    record: &Node,
    change: Option<&ChangeId>,
    workdir: &Path,
    info: Info,
    capture: Option<&Path>,
    source: &Source,
) -> Vec<Edit> {
    let link = |to: Node, rel: Rel| Edit::Link {
        from: record.clone(),
        to,
        rel,
        origin: Origin::Auto,
    };
    let mut edits = vec![Edit::Observe {
        node: record.clone(),
        info,
    }];
    if let Some(change) = change {
        let node = Node::Change(change.clone());
        edits.push(link(node.clone(), Rel::Judged));
        if let ChangeId::Snapshot(_) = change {
            edits.push(Edit::Observe {
                node,
                info: Info {
                    record: capture.map(Path::to_path_buf),
                    workdir: Some(workdir.to_path_buf()),
                    head: change.head().map(str::to_owned),
                    ..Info::default()
                },
            });
        }
    }
    edits.extend(
        source
            .runs
            .iter()
            .map(|run| link(Node::Run(run.clone()), Rel::About)),
    );
    edits.extend(
        source
            .tasks
            .iter()
            .map(|task| link(Node::Task(task.clone()), Rel::About)),
    );
    if let (Some(run), Some(change)) = (&source.produced_by, change) {
        edits.push(Edit::Link {
            from: Node::Run(run.clone()),
            to: Node::Change(change.clone()),
            rel: Rel::Produced,
            origin: Origin::Auto,
        });
    }
    edits
}

// ------------------------------------------------------------ navigation

/// What a row's button does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nav {
    /// `review --session=<dir>`.
    Review { dir: PathBuf },
    /// `verify --session=<dir>`.
    Verify { dir: PathBuf },
    /// In Tasks: the run's pane, else its worktree.
    Run(String),
    /// Copy this id (a SHA, a snapshot id).
    Copy(String),
    /// `tasks --repo=<main worktree> --select=<t-… | #N | path>`.
    Task { repo: PathBuf, select: String },
}

/// A sibling widget program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sibling {
    pub title: &'static str,
    /// Its file name in `Noa.app`'s `Contents/Helpers` (and `libexec/noa`).
    pub bundled: &'static str,
    /// The binary `cargo build` makes, and its directory under `widgets/`.
    pub crate_name: &'static str,
}

pub const REVIEW: Sibling = Sibling {
    title: "Review",
    bundled: "Noa Review",
    crate_name: "review",
};

pub const VERIFY: Sibling = Sibling {
    title: "Verify",
    bundled: "Noa Verify",
    crate_name: "verify",
};

pub const TASKS: Sibling = Sibling {
    title: "Tasks",
    bundled: "Noa Tasks",
    crate_name: "tasks",
};

impl Nav {
    /// The stored view of review `id`, its dir rebuilt from the id; `None`
    /// for an id not of the `rev-<millis>[-n]` form.
    pub fn review(data: &Path, id: &str) -> Option<Nav> {
        review_dir(data, id).map(|dir| Nav::Review { dir })
    }

    /// The stored view of verification `id` (see [`Nav::review`]).
    pub fn verify(data: &Path, id: &str) -> Option<Nav> {
        verify_dir(data, id).map(|dir| Nav::Verify { dir })
    }

    /// The detail of `task` in Tasks on the main worktree `repo`.
    pub fn task(repo: &Path, task: &TaskKey) -> Nav {
        Nav::Task {
            repo: repo.to_path_buf(),
            select: task.select(),
        }
    }

    /// A row's default: a review or verification opens its stored view, a
    /// run its pane, a change is copied, a task opens in Tasks. `None` for
    /// a session id of the wrong form.
    pub fn of(node: &Node, data: &Path, repo: &Path) -> Option<Nav> {
        match node {
            Node::Task(task) => Some(Nav::task(repo, task)),
            Node::Run(run) => Some(Nav::Run(run.clone())),
            Node::Change(change) => Some(Nav::Copy(change.as_str().to_owned())),
            Node::Review(id) => Nav::review(data, id),
            Node::Verify(id) => Nav::verify(data, id),
        }
    }

    /// The program it starts; `None` for what the widget does itself.
    pub fn sibling(&self) -> Option<Sibling> {
        match self {
            Nav::Review { .. } => Some(REVIEW),
            Nav::Verify { .. } => Some(VERIFY),
            Nav::Task { .. } => Some(TASKS),
            Nav::Run(_) | Nav::Copy(_) => None,
        }
    }

    /// The program's arguments (empty for what the widget does itself).
    pub fn argv(&self) -> Vec<String> {
        match self {
            Nav::Review { dir } | Nav::Verify { dir } => {
                vec![format!("--session={}", dir.display())]
            }
            Nav::Task { repo, select } => vec![
                format!("--repo={}", repo.display()),
                format!("--select={select}"),
            ],
            Nav::Run(_) | Nav::Copy(_) => Vec::new(),
        }
    }
}

/// Where `sibling` may be, given the directory of this widget's own
/// executable, in order: `<exe dir>/<bundled name>` (Noa.app's Helpers,
/// libexec/noa), then a development build
/// `<exe dir>/../../../<crate>/target/<profile>/<crate>` (this widget's
/// executable at `widgets/<self>/target/<profile>/`), its own profile first.
pub fn sibling_candidates(exe_dir: &Path, sibling: Sibling) -> Vec<PathBuf> {
    let mut out = vec![exe_dir.join(sibling.bundled)];
    if let Some(widgets) = exe_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
    {
        let target = widgets.join(sibling.crate_name).join("target");
        let profiles = if cfg!(debug_assertions) {
            ["debug", "release"]
        } else {
            ["release", "debug"]
        };
        out.extend(
            profiles
                .iter()
                .map(|profile| target.join(profile).join(sibling.crate_name)),
        );
    }
    out
}

/// The first of [`sibling_candidates`] that is a file.
pub fn locate_sibling(exe_dir: &Path, sibling: Sibling) -> Option<PathBuf> {
    sibling_candidates(exe_dir, sibling)
        .into_iter()
        .find(|path| path.is_file())
}

/// Starts the program `nav` names, detached (its own process group), and
/// reaps it on a thread. `Err` is the notice: the program could not be
/// found (naming the command to run) or started, or `nav` is one the
/// widget handles itself.
pub fn launch(nav: &Nav) -> Result<(), String> {
    use std::os::unix::process::CommandExt as _;
    let Some(sibling) = nav.sibling() else {
        return Err("このリンクは別のウィンドウでは開けません".to_owned());
    };
    let args = nav.argv();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let Some(program) = exe_dir.and_then(|dir| locate_sibling(&dir, sibling)) else {
        let command: Vec<String> = std::iter::once(sibling.crate_name.to_owned())
            .chain(args.iter().cloned())
            .map(|word| shell_word(&word))
            .collect();
        return Err(format!(
            "{} が見つかりません（widgets/{} をビルドしてください）。開くには: {}",
            sibling.bundled,
            sibling.crate_name,
            command.join(" ")
        ));
    };
    let mut command = Command::new(&program);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(home) = std::env::var_os("HOME") {
        command.current_dir(home);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("{} を起動できません: {e}", sibling.title))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// `word` as a shell reads it back: as it is when plain, else in single
/// quotes.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_=./#:+@%,".contains(&b));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A directory under the system temp dir, removed with everything in
    /// it when dropped; its canonical path, so it compares equal to what
    /// git prints.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(name: &str) -> Tmp {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "noa-links-{}-{name}-{}-{seq}",
                env!("CARGO_PKG_NAME"),
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Tmp(path.canonicalize().unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// `git -C dir <args>` for a test: no hooks, no signing, a fixed
    /// author; its stdout, trimmed. Panics when git fails.
    fn sh(dir: &Path, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        without_git_env(&mut cmd);
        let output = cmd
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "init.defaultBranch=main",
            ])
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A repository at `dir` with one commit of `a.txt`; its SHA.
    fn repo_at(dir: &Path) -> String {
        fs::create_dir_all(dir).unwrap();
        sh(dir, &["init", "-q"]);
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        commit(dir, "first")
    }

    /// Commits everything in `dir`; the new SHA.
    fn commit(dir: &Path, message: &str) -> String {
        sh(dir, &["add", "-A"]);
        sh(dir, &["commit", "-q", "--allow-empty", "-m", message]);
        sh(dir, &["rev-parse", "HEAD"])
    }

    fn task(id: &str) -> TaskKey {
        TaskKey::Local(id.into())
    }

    fn run(id: &str) -> Node {
        Node::Run(id.into())
    }

    fn commit_node(sha: &str) -> Node {
        Node::Change(ChangeId::Commit(sha.into()))
    }

    fn sha(n: u8) -> String {
        format!("{n:x}").repeat(40)[..40].to_owned()
    }

    fn auto(from: Node, to: Node, rel: Rel) -> Edit {
        Edit::Link {
            from,
            to,
            rel,
            origin: Origin::Auto,
        }
    }

    fn manual(from: Node, to: Node, rel: Rel) -> Edit {
        Edit::Link {
            from,
            to,
            rel,
            origin: Origin::Manual,
        }
    }

    fn observe(node: Node, info: Info) -> Edit {
        Edit::Observe { node, info }
    }

    fn linked(outcome: &Outcome) -> LinkId {
        match outcome {
            Outcome::Linked(id) => id.clone(),
            other => panic!("not linked: {other:?}"),
        }
    }

    fn log_in(tmp: &Tmp) -> PathBuf {
        tmp.path().join("links").join("repo-0").join(LINKS_FILE)
    }

    /// The current change `change`, taken from `run`.
    fn at(run: &str, change: &ChangeId) -> Current {
        Current {
            run: run.into(),
            change: change.clone(),
        }
    }

    /// A linked worktree of `main` at `path` on a new branch `branch`.
    fn worktree(main: &Path, path: &Path, branch: &str) {
        sh(
            main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().unwrap(),
            ],
        );
    }

    /// A task with run-1 (v1) and run-2 (v2), a review of v1 and a
    /// verification of v2.
    fn history(log: &Path) {
        let t = Node::Task(task("t-1"));
        let v1 = commit_node(&sha(1));
        let v2 = commit_node(&sha(2));
        let rev = Node::Review("rev-10".into());
        let ver = Node::Verify("ver-20".into());
        let edits = [
            auto(t.clone(), run("run-2"), Rel::Ran),
            auto(t.clone(), run("run-1"), Rel::Ran),
            auto(run("run-1"), v1.clone(), Rel::Produced),
            auto(run("run-2"), v2.clone(), Rel::Produced),
            auto(rev.clone(), v1, Rel::Judged),
            auto(rev.clone(), t.clone(), Rel::About),
            auto(ver.clone(), v2, Rel::Judged),
            auto(ver, run("run-2"), Rel::About),
            observe(
                rev,
                Info {
                    label: Some("差分 main..HEAD".into()),
                    outcome: Some("承認".into()),
                    ..Info::default()
                },
            ),
        ];
        let outcomes = append(log, &edits, 100).unwrap();
        assert!(
            outcomes
                .iter()
                .all(|o| matches!(o, Outcome::Linked(_) | Outcome::Observed)),
            "{outcomes:?}"
        );
    }

    #[test]
    fn written_links_reload_and_answer_the_tasks_query() {
        let tmp = Tmp::new("reload");
        let log = log_in(&tmp);
        history(&log);
        let set = replay(&log).unwrap();
        assert_eq!(set.links.len(), 8);
        let v2 = ChangeId::Commit(sha(2));
        let related = related(&set, &task("t-1"), Some(&at("run-2", &v2)));
        // Oldest run first, whichever was linked first.
        let runs: Vec<&str> = related.runs.iter().map(|r| r.run.as_str()).collect();
        assert_eq!(runs, ["run-1", "run-2"]);
        let r1 = &related.runs[0].changes;
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].change, ChangeId::Commit(sha(1)));
        assert_eq!(r1[0].results[0].node, Node::Review("rev-10".into()));
        assert_eq!(r1[0].results[0].info.outcome.as_deref(), Some("承認"));
        let r2 = &related.runs[1].changes;
        assert_eq!(r2[0].results[0].node, Node::Verify("ver-20".into()));
        assert!(r2[0].current);
        assert!(related.loose.is_empty(), "{:?}", related.loose);
        // Another task has none of it.
        assert_eq!(super::related(&set, &task("t-2"), None), Related::default());
    }

    #[test]
    fn links_are_many_to_many() {
        let tmp = Tmp::new("m2m");
        let log = log_in(&tmp);
        let (t1, t2) = (Node::Task(task("t-1")), Node::Task(task("t-2")));
        let c = commit_node(&sha(3));
        let edits = [
            auto(t1.clone(), run("run-1"), Rel::Ran),
            auto(t1.clone(), run("run-2"), Rel::Ran),
            // One run on two tasks.
            auto(t2.clone(), run("run-2"), Rel::Ran),
            auto(run("run-2"), c.clone(), Rel::Produced),
            auto(Node::Review("rev-1".into()), c.clone(), Rel::Judged),
            auto(Node::Review("rev-2".into()), c.clone(), Rel::Judged),
            auto(Node::Verify("ver-1".into()), c.clone(), Rel::Judged),
            // One review about two tasks.
            auto(Node::Review("rev-1".into()), t1, Rel::About),
            auto(Node::Review("rev-1".into()), t2, Rel::About),
        ];
        append(&log, &edits, 5).unwrap();
        let set = replay(&log).unwrap();
        let one = related(&set, &task("t-1"), None);
        assert_eq!(one.runs.len(), 2);
        assert_eq!(one.runs[1].changes[0].results.len(), 3);
        let two = related(&set, &task("t-2"), None);
        assert_eq!(two.runs.len(), 1);
        assert_eq!(two.runs[0].changes[0].results.len(), 3);
        let back = backlinks(&set, &Node::Review("rev-1".into()));
        let tasks: Vec<&TaskKey> = back.tasks.iter().map(|t| &t.task).collect();
        assert_eq!(tasks, [&task("t-1"), &task("t-2")]);
        // Every id differs, though all were taken at one millisecond.
        assert_eq!(set.ids.len(), 9);
        assert!(set.ids.contains(&LinkId::new("lnk-5")));
        assert!(set.ids.contains(&LinkId::new("lnk-5-9")));
    }

    #[test]
    fn a_torn_last_line_is_skipped_and_ended_by_the_next_append() {
        let tmp = Tmp::new("torn");
        let log = log_in(&tmp);
        append(
            &log,
            &[auto(Node::Task(task("t-1")), run("run-1"), Rel::Ran)],
            1,
        )
        .unwrap();
        let mut file = OpenOptions::new().append(true).open(&log).unwrap();
        file.write_all(br#"{"event":"Linked","id":"lnk-2","fr"#)
            .unwrap();
        drop(file);
        assert_eq!(replay(&log).unwrap().links.len(), 1);
        append(
            &log,
            &[auto(Node::Task(task("t-1")), run("run-2"), Rel::Ran)],
            3,
        )
        .unwrap();
        let set = replay(&log).unwrap();
        assert_eq!(set.links.len(), 2);
        let text = fs::read_to_string(&log).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.ends_with("\n"));
    }

    #[test]
    fn a_line_with_a_pair_its_relation_does_not_allow_is_skipped_and_refused() {
        let tmp = Tmp::new("pair");
        let log = log_in(&tmp);
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        // Written by hand: a review that "ran" a task.
        let bad = LinkEvent::Linked {
            id: LinkId::new("lnk-1"),
            from: Node::Review("rev-1".into()),
            to: Node::Task(task("t-1")),
            rel: Rel::Ran,
            origin: Origin::Manual,
            at: 1,
        };
        let good = LinkEvent::Linked {
            id: LinkId::new("lnk-2"),
            from: Node::Task(task("t-1")),
            to: run("run-1"),
            rel: Rel::Ran,
            origin: Origin::Auto,
            at: 2,
        };
        let lines = [&bad, &good]
            .map(|e| serde_json::to_string(e).unwrap())
            .join("\n");
        fs::write(&log, format!("{lines}\n")).unwrap();
        let set = replay(&log).unwrap();
        assert_eq!(set.links.len(), 1);
        assert_eq!(set.links[0].id, LinkId::new("lnk-2"));
        let outcomes = append(
            &log,
            &[
                auto(
                    Node::Review("rev-1".into()),
                    Node::Task(task("t-1")),
                    Rel::Ran,
                ),
                auto(
                    Node::Task(task("t-1")),
                    Node::Review("rev-1".into()),
                    Rel::About,
                ),
                auto(commit_node(&sha(1)), run("run-1"), Rel::Produced),
            ],
            3,
        )
        .unwrap();
        assert!(outcomes.iter().all(|o| matches!(o, Outcome::Refused(_))));
        assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 2);
    }

    #[test]
    fn the_lock_is_exclusive_and_racing_writers_record_a_link_once() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let tmp = Tmp::new("lock");
        let log = log_in(&tmp);
        let held = lock(&log).unwrap();
        let taken = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (log, taken) = (log.clone(), taken.clone());
            std::thread::spawn(move || {
                let _lock = lock(&log).unwrap();
                taken.store(true, Ordering::SeqCst);
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(!taken.load(Ordering::SeqCst), "a second lock was taken");
        drop(held);
        waiter.join().unwrap();
        assert!(taken.load(Ordering::SeqCst));

        let writers: Vec<_> = (0..8)
            .map(|_| {
                let log = log.clone();
                std::thread::spawn(move || {
                    let edit = auto(Node::Task(task("t-1")), run("run-1"), Rel::Ran);
                    append(&log, &[edit], 7).unwrap()
                })
            })
            .collect();
        let outcomes: Vec<Outcome> = writers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| matches!(o, Outcome::Linked(_)))
                .count(),
            1
        );
        assert_eq!(replay(&log).unwrap().links.len(), 1);
    }

    #[test]
    fn the_store_is_keyed_by_the_main_worktree_and_survives_worktree_remove() {
        let tmp = Tmp::new("survive");
        let main = tmp.path().join("app");
        repo_at(&main);
        let wt = tmp.path().join("app-wt").join("task-run-1");
        sh(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "task-run-1",
                wt.to_str().unwrap(),
            ],
        );
        let root = tmp.path().join("data").join("links");
        let log = path(&root, &wt).unwrap();
        assert_eq!(log, path(&root, &main).unwrap());
        assert_eq!(log, root.join(repo_key(&main)).join(LINKS_FILE));
        fs::write(wt.join("b.txt"), "two\n").unwrap();
        let snap = current_change(&wt).unwrap();
        let capture = wt.join(".noa/reviews/rev-1/target");
        fs::create_dir_all(&capture).unwrap();
        append(
            &log,
            &[auto(Node::Task(task("t-1")), run("run-1"), Rel::Ran)],
            1,
        )
        .unwrap();
        record_handoff(
            &log,
            Node::Review("rev-1".into()),
            Some(snap.clone()),
            &wt,
            Info::default(),
            Some(&capture),
        )
        .unwrap();
        sh(
            &main,
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
        );
        assert!(!wt.exists());
        let set = replay(&path(&root, &main).unwrap()).unwrap();
        assert_eq!(set.links.len(), 2);
        // The snapshot's capture dir went with the worktree: missing, its
        // link kept.
        let data = tmp.path().join("data");
        assert_eq!(
            presence(&set, &Node::Change(snap.clone()), &data, &main),
            Presence::Missing {
                what: format!("{} {}", snap.as_str(), capture.display())
            }
        );
        assert!(
            set.live(
                &Node::Review("rev-1".into()),
                &Node::Change(snap),
                Rel::Judged
            )
            .is_some()
        );
    }

    #[test]
    fn an_unlinked_automatic_link_is_not_recorded_again_but_a_manual_add_restores_it() {
        let tmp = Tmp::new("dedupe");
        let log = log_in(&tmp);
        let edit = auto(Node::Task(task("t-1")), run("run-1"), Rel::Ran);
        let id = linked(&append(&log, std::slice::from_ref(&edit), 1).unwrap()[0]);
        assert_eq!(
            append(&log, std::slice::from_ref(&edit), 2).unwrap(),
            [Outcome::Skipped]
        );
        assert_eq!(
            append(&log, &[Edit::Unlink(id.clone())], 3).unwrap(),
            [Outcome::Unlinked]
        );
        assert_eq!(
            append(&log, &[Edit::Unlink(id)], 4).unwrap(),
            [Outcome::Skipped]
        );
        assert_eq!(
            append(&log, std::slice::from_ref(&edit), 5).unwrap(),
            [Outcome::Skipped]
        );
        let set = replay(&log).unwrap();
        assert!(set.links.is_empty());
        assert!(set.ever(&Node::Task(task("t-1")), &run("run-1"), Rel::Ran));
        // A person adds it back; then automatic recording still skips it.
        let again = manual(Node::Task(task("t-1")), run("run-1"), Rel::Ran);
        linked(&append(&log, &[again], 6).unwrap()[0]);
        assert_eq!(append(&log, &[edit], 7).unwrap(), [Outcome::Skipped]);
        let set = replay(&log).unwrap();
        assert_eq!(set.links.len(), 1);
        assert_eq!(set.links[0].origin, Origin::Manual);
        // An observation the node already has is not written again.
        let info = Info {
            label: Some("claude · task-run-1".into()),
            ..Info::default()
        };
        assert_eq!(
            append(&log, &[observe(run("run-1"), info.clone())], 8).unwrap(),
            [Outcome::Observed]
        );
        assert_eq!(
            append(&log, &[observe(run("run-1"), info)], 9).unwrap(),
            [Outcome::Skipped]
        );
        let lines = fs::read_to_string(&log).unwrap().lines().count();
        assert_eq!(lines, 4);
    }

    #[test]
    fn a_manual_add_is_written_once_and_a_duplicate_is_refused() {
        let tmp = Tmp::new("manual");
        let log = log_in(&tmp);
        let t = Node::Task(task("t-1"));
        let edits = [
            manual(Node::Review("rev-1".into()), t.clone(), Rel::About),
            manual(t.clone(), commit_node(&sha(4)), Rel::Produced),
            manual(t.clone(), run("run-3"), Rel::Ran),
            manual(Node::Review("rev-1".into()), t.clone(), Rel::About),
        ];
        let outcomes = append(&log, &edits, 1).unwrap();
        assert!(matches!(outcomes[0], Outcome::Linked(_)));
        assert!(matches!(outcomes[1], Outcome::Linked(_)));
        assert!(matches!(outcomes[2], Outcome::Linked(_)));
        assert_eq!(outcomes[3], Outcome::Refused(ALREADY_LINKED.to_owned()));
        assert_eq!(
            append(&log, &edits[..1], 2).unwrap(),
            [Outcome::Refused(ALREADY_LINKED.to_owned())]
        );
        let set = replay(&log).unwrap();
        assert_eq!(set.links.len(), 3);
        let related = related(&set, &task("t-1"), None);
        assert_eq!(related.changes[0].change, ChangeId::Commit(sha(4)));
        assert_eq!(related.runs[0].run, "run-3");
        // The review judged nothing: loose, its About link named.
        assert_eq!(related.loose.len(), 1);
        assert_eq!(related.loose[0].judged, None);
        assert!(!related.loose[0].stale);
    }

    #[test]
    fn a_result_on_v1_is_stale_once_v2_is_current_and_v2_shows_no_result_yet() {
        let set = fold(&[
            LinkEvent::Linked {
                id: LinkId::new("lnk-1"),
                from: Node::Task(task("t-1")),
                to: run("run-1"),
                rel: Rel::Ran,
                origin: Origin::Auto,
                at: 1,
            },
            LinkEvent::Linked {
                id: LinkId::new("lnk-2"),
                from: run("run-1"),
                to: commit_node(&sha(1)),
                rel: Rel::Produced,
                origin: Origin::Auto,
                at: 2,
            },
            LinkEvent::Linked {
                id: LinkId::new("lnk-3"),
                from: Node::Review("rev-1".into()),
                to: commit_node(&sha(1)),
                rel: Rel::Judged,
                origin: Origin::Auto,
                at: 3,
            },
        ]);
        let v1 = ChangeId::Commit(sha(1));
        let v2 = ChangeId::Commit(sha(2));
        let related = related(&set, &task("t-1"), Some(&at("run-1", &v2)));
        let changes = &related.runs[0].changes;
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].change, v1);
        assert!(!changes[0].current);
        assert!(changes[0].results[0].stale);
        assert_eq!(changes[0].results[0].judged, Some(v1.clone()));
        assert_eq!(
            changes[1],
            ChangeEntry {
                change: v2,
                link: None,
                current: true,
                results: Vec::new(),
            }
        );
        // At v1, the same result is current.
        let at_v1 = super::related(&set, &task("t-1"), Some(&at("run-1", &v1)));
        assert_eq!(at_v1.runs[0].changes.len(), 1);
        assert!(at_v1.runs[0].changes[0].current);
        assert!(!at_v1.runs[0].changes[0].results[0].stale);
        // With no current change, nothing is stale or current.
        let none = super::related(&set, &task("t-1"), None);
        assert!(!none.runs[0].changes[0].current);
        assert!(!none.runs[0].changes[0].results[0].stale);
    }

    #[test]
    fn a_branch_that_moves_on_leaves_the_result_on_the_commit_it_judged() {
        let tmp = Tmp::new("moved");
        let main = tmp.path().join("app");
        repo_at(&main);
        let wt = tmp.path().join("app-wt").join("task-run-1");
        sh(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "task-run-1",
                wt.to_str().unwrap(),
            ],
        );
        let log = tmp.path().join("links.jsonl");
        let start = sh(&wt, &["rev-parse", "HEAD"]);
        append(
            &log,
            &[
                auto(Node::Task(task("t-1")), run("run-1"), Rel::Ran),
                observe(
                    run("run-1"),
                    Info {
                        workdir: Some(wt.clone()),
                        branch: Some("task-run-1".into()),
                        head: Some(start),
                        ..Info::default()
                    },
                ),
            ],
            1,
        )
        .unwrap();
        fs::write(wt.join("a.txt"), "v1\n").unwrap();
        let v1 = commit(&wt, "v1");
        let judged = current_change(&wt).unwrap();
        assert_eq!(judged, ChangeId::Commit(v1.clone()));
        let source = record_handoff(
            &log,
            Node::Review("rev-1".into()),
            Some(judged),
            &wt,
            Info::default(),
            None,
        )
        .unwrap()
        .source;
        // Rule 2: the run's own worktree.
        assert_eq!(source.produced_by.as_deref(), Some("run-1"));
        fs::write(wt.join("a.txt"), "v2\n").unwrap();
        let v2 = commit(&wt, "v2");
        let set = replay(&log).unwrap();
        let now = task_current(&set, &task("t-1"), &main).unwrap();
        assert_eq!(now, at("run-1", &ChangeId::Commit(v2.clone())));
        let related = related(&set, &task("t-1"), Some(&now));
        let changes = &related.runs[0].changes;
        assert_eq!(changes[0].change, ChangeId::Commit(v1));
        assert!(changes[0].results[0].stale);
        assert_eq!(changes[1].change, ChangeId::Commit(v2));
        assert!(changes[1].results.is_empty());
        // Once the worktree is gone, the current change is the last one
        // the run produced (v2 was never recorded).
        sh(
            &main,
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
        );
        let set = replay(&log).unwrap();
        assert_eq!(
            task_current(&set, &task("t-1"), &main),
            Some(at("run-1", &changes[0].change))
        );
    }

    #[test]
    fn snapshot_ids_name_exact_states_and_ignore_noa() {
        let tmp = Tmp::new("snap");
        let repo = tmp.path().join("app");
        let head = repo_at(&repo);
        // Clean: the commit, never a snapshot.
        assert_eq!(snapshot_id(&repo).unwrap(), ChangeId::Commit(head.clone()));
        // .noa/ edits do not make it dirty.
        fs::create_dir_all(repo.join(".noa/reviews/rev-1")).unwrap();
        fs::write(repo.join(".noa/reviews/rev-1/x"), "x").unwrap();
        fs::write(repo.join(".noa/.gitignore"), "*\n").unwrap();
        assert_eq!(snapshot_id(&repo).unwrap(), ChangeId::Commit(head.clone()));
        // Same dirty bytes, same id; from a subdirectory too.
        fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/new.rs"), "fn main() {}\n").unwrap();
        let first = snapshot_id(&repo).unwrap();
        let ChangeId::Snapshot(id) = &first else {
            panic!("{first:?}")
        };
        assert!(id.starts_with(&format!("snap-{head}-")), "{id}");
        assert_eq!(id.len(), "snap-".len() + 40 + 1 + 16);
        assert_eq!(first.head(), Some(head.as_str()));
        assert_eq!(snapshot_id(&repo).unwrap(), first);
        assert_eq!(snapshot_id(&repo.join("src")).unwrap(), first);
        fs::write(repo.join(".noa/reviews/rev-1/y"), "y").unwrap();
        assert_eq!(snapshot_id(&repo).unwrap(), first);
        // One byte more in a tracked file, or in an untracked one: another id.
        fs::write(repo.join("a.txt"), "one\ntwo!\n").unwrap();
        let tracked = snapshot_id(&repo).unwrap();
        assert_ne!(tracked, first);
        fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        assert_eq!(snapshot_id(&repo).unwrap(), first);
        fs::write(repo.join("src/new.rs"), "fn main() {}\n\n").unwrap();
        let untracked = snapshot_id(&repo).unwrap();
        assert_ne!(untracked, first);
        assert_ne!(untracked, tracked);
        // Back to the same bytes: the same id again.
        fs::write(repo.join("src/new.rs"), "fn main() {}\n").unwrap();
        assert_eq!(snapshot_id(&repo).unwrap(), first);
        // A snapshot on v1's head is not v1.
        assert_ne!(first, ChangeId::Commit(head));
        assert_eq!(current_change(&repo).unwrap(), first);
    }

    #[test]
    fn an_unborn_head_or_a_tree_over_the_cap_has_no_snapshot() {
        let tmp = Tmp::new("snap-none");
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        sh(&empty, &["init", "-q"]);
        assert!(snapshot_id(&empty).is_err());
        let repo = tmp.path().join("app");
        repo_at(&repo);
        let big = vec![b'x'; SNAPSHOT_MAX_BYTES as usize + 1];
        fs::write(repo.join("big.bin"), big).unwrap();
        assert_eq!(snapshot_id(&repo), Err(TOO_LARGE.to_owned()));
        assert!(snapshot_id(tmp.path()).is_err());
    }

    #[test]
    fn an_untracked_entrys_kind_and_execute_bit_are_part_of_the_snapshot() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = Tmp::new("snap-kind");
        let repo = tmp.path().join("app");
        repo_at(&repo);
        let path = repo.join("run");
        let file = |mode: u32| {
            let _ = fs::remove_file(&path);
            fs::write(&path, "target").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        };
        file(0o644);
        let plain = snapshot_id(&repo).unwrap();
        file(0o755);
        let executable = snapshot_id(&repo).unwrap();
        // A link whose target is the file's text.
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("target", &path).unwrap();
        let link = snapshot_id(&repo).unwrap();
        assert_ne!(plain, executable);
        assert_ne!(plain, link);
        assert_ne!(executable, link);
        file(0o644);
        assert_eq!(snapshot_id(&repo).unwrap(), plain);
    }

    #[test]
    fn a_submodule_with_changes_of_its_own_or_an_untracked_repository_has_no_snapshot() {
        let tmp = Tmp::new("snap-nested");
        let sub = tmp.path().join("sub");
        repo_at(&sub);
        let repo = tmp.path().join("app");
        repo_at(&repo);
        sh(
            &repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().unwrap(),
                "sub",
            ],
        );
        let head = commit(&repo, "sub");
        assert_eq!(snapshot_id(&repo).unwrap(), ChangeId::Commit(head));
        // A tracked file changed inside it, or an untracked one: the
        // parent's diff names both as `<commit>-dirty`.
        let inside = repo.join("sub");
        fs::write(inside.join("a.txt"), "changed\n").unwrap();
        assert_eq!(snapshot_id(&repo), Err(NESTED.to_owned()));
        sh(&inside, &["checkout", "-q", "--", "a.txt"]);
        fs::write(inside.join("new.txt"), "new\n").unwrap();
        assert_eq!(snapshot_id(&repo), Err(NESTED.to_owned()));
        fs::remove_file(inside.join("new.txt")).unwrap();
        // A new commit in it, its work tree clean: the diff names that
        // commit, whatever diff.submodule says.
        fs::write(inside.join("a.txt"), "two\n").unwrap();
        commit(&inside, "two");
        let moved = snapshot_id(&repo).unwrap();
        assert!(matches!(moved, ChangeId::Snapshot(_)), "{moved:?}");
        sh(&repo, &["config", "diff.submodule", "log"]);
        assert_eq!(snapshot_id(&repo).unwrap(), moved);
        // An untracked repository in the tree.
        repo_at(&repo.join("nested"));
        assert_eq!(snapshot_id(&repo), Err(NESTED.to_owned()));
    }

    #[test]
    fn a_head_that_moves_while_the_state_is_read_gives_no_snapshot() {
        let tmp = Tmp::new("snap-moved");
        let repo = tmp.path().join("app");
        repo_at(&repo);
        fs::write(repo.join("a.txt"), "two\n").unwrap();
        fs::write(repo.join("b.txt"), "b\n").unwrap();
        // Everything committed meanwhile: clean on the new HEAD.
        let moved = snapshot_id_with(&repo, || {
            commit(&repo, "all");
        });
        assert_eq!(moved, Err(HEAD_MOVED.to_owned()));
        // Part of it committed: still dirty, on the new HEAD.
        fs::write(repo.join("a.txt"), "three\n").unwrap();
        fs::write(repo.join("b.txt"), "bb\n").unwrap();
        let moved = snapshot_id_with(&repo, || {
            sh(&repo, &["commit", "-q", "-m", "a", "--", "a.txt"]);
        });
        assert_eq!(moved, Err(HEAD_MOVED.to_owned()));
        assert!(matches!(
            snapshot_id_with(&repo, || {}),
            Ok(ChangeId::Snapshot(_))
        ));
    }

    #[test]
    fn diff_settings_do_not_change_a_snapshot_id() {
        let tmp = Tmp::new("snap-config");
        let repo = tmp.path().join("app");
        fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q"]);
        fs::write(
            repo.join("a.txt"),
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n\n15\n",
        )
        .unwrap();
        fs::write(repo.join("b.txt"), "b\n").unwrap();
        commit(&repo, "first");
        // Two hunks eight lines apart, a blank context line, two files.
        fs::write(
            repo.join("a.txt"),
            "1\nX\n3\n4\n5\n6\n7\n8\n9\n10\nY\n12\n13\n14\n\n15\nZ\n",
        )
        .unwrap();
        fs::write(repo.join("b.txt"), "bb\n").unwrap();
        let before = snapshot_id(&repo).unwrap();
        // git follows the order file where nothing pins it: b.txt first.
        let order = tmp.path().join("order");
        fs::write(&order, "b.txt\na.txt\n").unwrap();
        sh(
            &repo,
            &["config", "diff.orderFile", order.to_str().unwrap()],
        );
        let plain = sh(&repo, &["diff", "HEAD"]);
        assert!(
            plain.find("b.txt").unwrap() < plain.find("a.txt").unwrap(),
            "{plain}"
        );
        for (key, value) in [
            ("diff.interHunkContext", "10"),
            ("diff.suppressBlankEmpty", "true"),
            ("diff.algorithm", "patience"),
            ("diff.indentHeuristic", "false"),
            ("diff.context", "10"),
            ("diff.noprefix", "true"),
            ("diff.mnemonicPrefix", "true"),
            ("diff.srcPrefix", "x/"),
            ("diff.dstPrefix", "y/"),
            ("diff.relative", "true"),
            ("diff.renames", "copies"),
            ("diff.external", "/bin/false"),
            ("core.bigFileThreshold", "1"),
            ("core.quotePath", "true"),
            ("color.ui", "always"),
            ("color.diff", "always"),
        ] {
            sh(&repo, &["config", key, value]);
        }
        assert_eq!(snapshot_id(&repo).unwrap(), before);
    }

    #[test]
    fn the_current_change_never_writes_the_index() {
        let tmp = Tmp::new("snap-index");
        let repo = tmp.path().join("app");
        repo_at(&repo);
        fs::write(repo.join("b.txt"), "b\n").unwrap();
        commit(&repo, "b");
        // b.txt's bytes are as committed but its mtime is not what the
        // index holds (git diff refreshes such an entry); a.txt changed,
        // so the tree has a snapshot and the diff runs.
        fs::File::options()
            .write(true)
            .open(repo.join("b.txt"))
            .unwrap()
            .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000))
            .unwrap();
        fs::write(repo.join("a.txt"), "two\n").unwrap();
        let index = repo.join(".git/index");
        let before = fs::read(&index).unwrap();
        let id = current_change(&repo).unwrap();
        assert!(matches!(id, ChangeId::Snapshot(_)), "{id:?}");
        assert_eq!(fs::read(&index).unwrap(), before);
        // A git that may write the index does rewrite it (the stat info
        // was stale), and the state keeps its id.
        sh(&repo, &["status", "--porcelain"]);
        assert_ne!(fs::read(&index).unwrap(), before);
        assert_eq!(current_change(&repo).unwrap(), id);
    }

    #[test]
    fn a_missing_record_is_marked_missing_and_its_links_are_kept() {
        let tmp = Tmp::new("presence");
        let main = tmp.path().join("app");
        let head = repo_at(&main);
        let data = tmp.path().join("data");
        let runs_log = data.join("tasks").join("app-0").join("runs.jsonl");
        fs::create_dir_all(runs_log.parent().unwrap()).unwrap();
        fs::write(
            &runs_log,
            r#"{"event":"RunStarted","run":"run-1","task":{"local":"t-1"},"agent":"claude","workdir":"/w","branch":null,"label":"task:run-1","at":1}"#.to_owned() + "\n",
        )
        .unwrap();
        let review = data.join("reviews").join("rev-1");
        fs::create_dir_all(&review).unwrap();
        fs::write(review.join("events.jsonl"), "").unwrap();
        fs::create_dir_all(main.join(".noa/tasks")).unwrap();
        fs::write(main.join(".noa/tasks/t-1.md"), "# one\n").unwrap();
        let t1 = Node::Task(task("t-1"));
        let gone_sha = "0".repeat(40);
        let log = log_in(&tmp);
        append(
            &log,
            &[
                auto(t1.clone(), run("run-1"), Rel::Ran),
                auto(t1.clone(), run("run-2"), Rel::Ran),
                auto(run("run-1"), commit_node(&head), Rel::Produced),
                auto(run("run-2"), commit_node(&gone_sha), Rel::Produced),
                auto(
                    Node::Review("rev-1".into()),
                    commit_node(&head),
                    Rel::Judged,
                ),
                auto(
                    Node::Review("rev-2".into()),
                    commit_node(&head),
                    Rel::Judged,
                ),
                auto(Node::Verify("ver-1".into()), t1.clone(), Rel::About),
                observe(
                    run("run-1"),
                    Info {
                        record: Some(runs_log.clone()),
                        ..Info::default()
                    },
                ),
                observe(
                    run("run-2"),
                    Info {
                        record: Some(runs_log.clone()),
                        ..Info::default()
                    },
                ),
            ],
            1,
        )
        .unwrap();
        let set = replay(&log).unwrap();
        let found = presences(&set, &data, &main);
        assert_eq!(found[&t1], Presence::Found);
        assert_eq!(found[&run("run-1")], Presence::Found);
        assert_eq!(
            found[&run("run-2")],
            Presence::Missing {
                what: format!("run-2 {}", runs_log.display())
            }
        );
        assert_eq!(found[&commit_node(&head)], Presence::Found);
        assert_eq!(
            found[&commit_node(&gone_sha)],
            Presence::Missing { what: gone_sha }
        );
        assert_eq!(found[&Node::Review("rev-1".into())], Presence::Found);
        assert_eq!(
            found[&Node::Review("rev-2".into())],
            Presence::Missing {
                what: format!(
                    "rev-2 {}",
                    data.join("reviews/rev-2/events.jsonl").display()
                )
            }
        );
        assert!(matches!(
            found[&Node::Verify("ver-1".into())],
            Presence::Missing { .. }
        ));
        // A removed session dir: missing.
        fs::remove_dir_all(&review).unwrap();
        assert!(matches!(
            presence(&set, &Node::Review("rev-1".into()), &data, &main),
            Presence::Missing { .. }
        ));
        // A GitHub issue, a run with no runs log recorded, git failing:
        // not known.
        let issue = Node::Task(TaskKey::GitHub {
            repo: "o/n".into(),
            number: 1,
        });
        assert_eq!(presence(&set, &issue, &data, &main), Presence::Unknown);
        assert_eq!(
            presence(&set, &run("run-9"), &data, &main),
            Presence::Unknown
        );
        assert_eq!(
            presence(
                &set,
                &commit_node(&head),
                &data,
                &tmp.path().join("nowhere")
            ),
            Presence::Unknown
        );
        // Every row stays in 関連成果.
        let related = related(&set, &task("t-1"), None);
        assert_eq!(related.runs.len(), 2);
        assert_eq!(related.runs[0].changes[0].results.len(), 2);
        assert_eq!(
            related.runs[1].changes[0].change,
            ChangeId::Commit("0".repeat(40))
        );
        assert_eq!(related.loose[0].node, Node::Verify("ver-1".into()));
    }

    #[test]
    fn backlinks_name_the_task_the_run_and_the_judged_change() {
        let tmp = Tmp::new("back");
        let log = log_in(&tmp);
        history(&log);
        let set = replay(&log).unwrap();
        let back = backlinks(&set, &Node::Verify("ver-20".into()));
        assert!(back.tasks.is_empty());
        assert_eq!(back.runs[0].run, "run-2");
        assert_eq!(back.changes.len(), 1);
        assert_eq!(back.changes[0].change, ChangeId::Commit(sha(2)));
        assert_eq!(back.changes[0].tasks, [task("t-1")]);
        let back = backlinks(&set, &Node::Review("rev-10".into()));
        assert_eq!(back.tasks[0].task, task("t-1"));
        assert_eq!(
            backlinks(&set, &Node::Review("rev-99".into())),
            BackLinks::default()
        );
        // 開く on the task row.
        let nav = Nav::task(Path::new("/src/app"), &back.tasks[0].task);
        assert_eq!(nav.argv(), ["--repo=/src/app", "--select=t-1"]);
        assert_eq!(nav.sibling(), Some(TASKS));
    }

    #[test]
    fn navigation_argv_and_where_a_sibling_program_is_looked_for() {
        let data = Path::new("/data/noa");
        let nav = Nav::review(data, "rev-12").unwrap();
        assert_eq!(
            nav,
            Nav::Review {
                dir: PathBuf::from("/data/noa/reviews/rev-12")
            }
        );
        assert_eq!(nav.argv(), ["--session=/data/noa/reviews/rev-12"]);
        assert_eq!(nav.sibling(), Some(REVIEW));
        assert_eq!(
            Nav::verify(data, "ver-3-2").unwrap().argv(),
            ["--session=/data/noa/verifications/ver-3-2"]
        );
        for bad in ["rev-", "rev-1/../x", "../rev-1", "rev-1-", "rev-a", "ver-1"] {
            assert_eq!(Nav::review(data, bad), None, "{bad}");
        }
        let gh = TaskKey::GitHub {
            repo: "o/n".into(),
            number: 12,
        };
        assert_eq!(
            Nav::task(Path::new("/r"), &gh).argv(),
            ["--repo=/r", "--select=#12"]
        );
        let code = TaskKey::Code {
            path: "src/a.rs".into(),
            key: "k".into(),
        };
        assert_eq!(code.select(), "src/a.rs");
        assert_eq!(
            Nav::of(&commit_node(&sha(1)), data, Path::new("/r")),
            Some(Nav::Copy(sha(1)))
        );
        assert_eq!(
            Nav::of(&run("run-1"), data, Path::new("/r")),
            Some(Nav::Run("run-1".into()))
        );
        assert!(Nav::Run("run-1".into()).argv().is_empty());
        assert_eq!(Nav::Copy("x".into()).sibling(), None);
        assert!(launch(&Nav::Copy("x".into())).is_err());

        let bundle = Path::new("/Applications/Noa.app/Contents/Helpers");
        let (first, second) = if cfg!(debug_assertions) {
            ("debug", "release")
        } else {
            ("release", "debug")
        };
        assert_eq!(
            sibling_candidates(bundle, REVIEW),
            [
                bundle.join("Noa Review"),
                PathBuf::from(format!("/Applications/review/target/{first}/review")),
                PathBuf::from(format!("/Applications/review/target/{second}/review")),
            ]
        );
        let dev = Path::new("/src/noa/widgets/tasks/target/debug");
        assert_eq!(
            sibling_candidates(dev, VERIFY),
            [
                dev.join("Noa Verify"),
                PathBuf::from(format!("/src/noa/widgets/verify/target/{first}/verify")),
                PathBuf::from(format!("/src/noa/widgets/verify/target/{second}/verify")),
            ]
        );
        let tmp = Tmp::new("locate");
        let exe = tmp.path().join("tasks/target/debug");
        let built = tmp.path().join(format!("review/target/{second}"));
        fs::create_dir_all(&exe).unwrap();
        fs::create_dir_all(&built).unwrap();
        assert_eq!(locate_sibling(&exe, REVIEW), None);
        fs::write(built.join("review"), "").unwrap();
        assert_eq!(locate_sibling(&exe, REVIEW), Some(built.join("review")));
        fs::write(exe.join("Noa Review"), "").unwrap();
        assert_eq!(locate_sibling(&exe, REVIEW), Some(exe.join("Noa Review")));
        assert_eq!(
            shell_word("--session=/Library/Application Support/noa"),
            "'--session=/Library/Application Support/noa'"
        );
    }

    #[test]
    fn source_of_follows_rules_one_to_three_exactly() {
        let t1 = Node::Task(task("t-1"));
        let t2 = Node::Task(task("t-2"));
        let c = ChangeId::Commit(sha(5));
        // Every run below started on sha(4).
        let start = ChangeId::Commit(sha(4));
        let wt = PathBuf::from("/src/app-wt/task-run-2");
        let observed = |run: &str, workdir: &str, branch: Option<&str>| LinkEvent::Observed {
            node: Node::Run(run.into()),
            info: Info {
                workdir: Some(PathBuf::from(workdir)),
                branch: branch.map(str::to_owned),
                head: Some(sha(4)),
                ..Info::default()
            },
            at: 1,
        };
        let link = |id: &str, from: Node, to: Node, rel: Rel| LinkEvent::Linked {
            id: LinkId::new(id),
            from,
            to,
            rel,
            origin: Origin::Auto,
            at: 1,
        };
        let base = vec![
            link("lnk-1", t1.clone(), run("run-1"), Rel::Ran),
            link("lnk-2", t2.clone(), run("run-1"), Rel::Ran),
            link("lnk-3", t1.clone(), run("run-2"), Rel::Ran),
            observed("run-2", "/src/app-wt/task-run-2", Some("task-run-2")),
            // Runs made in the main checkout: no branch.
            observed("run-3", "/src/app", None),
            observed("run-4", "/src/app", None),
        ];
        // Rule 1: run-1 produced the change, whatever the work tree.
        let mut events = base.clone();
        events.push(link(
            "lnk-4",
            run("run-1"),
            Node::Change(c.clone()),
            Rel::Produced,
        ));
        let set = fold(&events);
        assert_eq!(
            source_of(&set, Some(&c), &wt, None),
            Source {
                runs: vec!["run-1".into()],
                tasks: vec![task("t-1"), task("t-2")],
                produced_by: None,
            }
        );
        // Rule 2: the one run whose own worktree it is, at that change.
        let set = fold(&base);
        let rule_two = Source {
            runs: vec!["run-2".into()],
            tasks: vec![task("t-1")],
            produced_by: Some("run-2".into()),
        };
        assert_eq!(source_of(&set, Some(&c), &wt, Some(&c)), rule_two);
        // … its HEAD commit, with uncommitted work on it, or that work.
        let snap = ChangeId::Snapshot(format!("snap-{}-00000000000000ff", sha(5)));
        assert_eq!(source_of(&set, Some(&c), &wt, Some(&snap)), rule_two);
        assert_eq!(source_of(&set, Some(&snap), &wt, Some(&snap)), rule_two);
        // … uncommitted work on the commit the run started on is its own.
        let on_start = ChangeId::Snapshot(format!("snap-{}-00000000000000ff", sha(4)));
        assert_eq!(
            source_of(&set, Some(&on_start), &wt, Some(&on_start)).produced_by,
            Some("run-2".into())
        );
        // Not rule 2: a commit the work tree is not at (an explicit ref), a
        // work tree git cannot read, the commit the run started on.
        let other = ChangeId::Commit(sha(6));
        assert_eq!(
            source_of(&set, Some(&other), &wt, Some(&c)),
            Source::default()
        );
        assert_eq!(
            source_of(&set, Some(&snap), &wt, Some(&c)),
            Source::default()
        );
        assert_eq!(source_of(&set, Some(&c), &wt, None), Source::default());
        assert_eq!(
            source_of(&set, Some(&start), &wt, Some(&start)),
            Source::default()
        );
        // With no change (a Files review): About, no Produced.
        assert_eq!(source_of(&set, None, &wt, None).produced_by, None);
        assert_eq!(source_of(&set, None, &wt, None).runs, ["run-2"]);
        // Rule 3: the main checkout (no branch), an unknown work tree.
        assert_eq!(
            source_of(&set, Some(&c), Path::new("/src/app"), Some(&c)),
            Source::default()
        );
        assert_eq!(
            source_of(&set, Some(&c), Path::new("/elsewhere"), Some(&c)),
            Source::default()
        );
        // Two runs claiming one worktree: none.
        let mut events = base;
        events.push(observed("run-5", "/src/app-wt/task-run-2", Some("x")));
        assert_eq!(
            source_of(&fold(&events), Some(&c), &wt, Some(&c)),
            Source::default()
        );
    }

    /// A repository `app` in `tmp` and a worktree of it for run-2 of t-1
    /// (branch `task-run-2`, observed with its start commit); the store
    /// log, the worktree and the start commit.
    fn run_in_worktree(tmp: &Tmp) -> (PathBuf, PathBuf, String) {
        let main = tmp.path().join("app");
        let start = repo_at(&main);
        let wt = tmp.path().join("app-wt/task-run-2");
        worktree(&main, &wt, "task-run-2");
        let log = log_in(tmp);
        append(
            &log,
            &[
                auto(Node::Task(task("t-1")), run("run-2"), Rel::Ran),
                observe(
                    run("run-2"),
                    Info {
                        workdir: Some(wt.clone()),
                        branch: Some("task-run-2".into()),
                        head: Some(start.clone()),
                        ..Info::default()
                    },
                ),
            ],
            1,
        )
        .unwrap();
        (log, wt, start)
    }

    #[test]
    fn record_handoff_writes_judged_about_and_produced_once() {
        let tmp = Tmp::new("handoff");
        let (log, wt, start) = run_in_worktree(&tmp);
        fs::write(wt.join("a.txt"), "wip\n").unwrap();
        let snap = current_change(&wt).unwrap();
        let capture = wt.join(".noa/verify/ver-1/target");
        let rec = Node::Verify("ver-1".into());
        let info = Info {
            label: Some("作業ツリー".into()),
            ..Info::default()
        };
        for _ in 0..2 {
            let source = record_handoff(
                &log,
                rec.clone(),
                Some(snap.clone()),
                &wt,
                info.clone(),
                Some(&capture),
            )
            .unwrap();
            assert_eq!(source.source.runs, ["run-2"]);
        }
        let set = replay(&log).unwrap();
        // Ran, Judged, About run, About task, Produced: once each.
        assert_eq!(set.links.len(), 5);
        let snap_info = set.info_of(&Node::Change(snap.clone()));
        assert_eq!(snap_info.record, Some(capture));
        assert_eq!(snap_info.head, Some(start));
        assert_eq!(set.info_of(&rec).label.as_deref(), Some("作業ツリー"));
        let back = backlinks(&set, &rec);
        assert_eq!(back.tasks[0].task, task("t-1"));
        assert_eq!(back.changes[0].change, snap);
        assert_eq!(back.changes[0].tasks, [task("t-1")]);
        // The next review of the same change finds it by rule 1.
        let src = source_of(&set, Some(&snap), Path::new("/x"), None);
        assert_eq!(
            (src.runs, src.produced_by),
            (vec!["run-2".to_owned()], None)
        );
    }

    #[test]
    fn a_review_of_a_commit_the_worktree_is_not_at_records_no_producer() {
        let tmp = Tmp::new("explicit-ref");
        let (log, wt, start) = run_in_worktree(&tmp);
        fs::write(wt.join("a.txt"), "v1\n").unwrap();
        let v1 = commit(&wt, "v1");
        fs::write(wt.join("a.txt"), "v2\n").unwrap();
        commit(&wt, "v2");
        // Review of the ref v1 (not where the worktree is) and of the
        // commit the run started on: no source, no producer.
        for (id, target) in [("rev-1", &v1), ("rev-2", &start)] {
            let source = record_handoff(
                &log,
                Node::Review(id.into()),
                Some(ChangeId::Commit(target.clone())),
                &wt,
                Info::default(),
                None,
            )
            .unwrap();
            assert_eq!(source.source, Source::default(), "{id}");
        }
        let set = replay(&log).unwrap();
        assert_eq!(set.links_to(&commit_node(&v1), Rel::Produced).count(), 0);
        assert_eq!(set.links_to(&commit_node(&start), Rel::Produced).count(), 0);
        // The commit it is at: rule 2.
        let head = sh(&wt, &["rev-parse", "HEAD"]);
        let source = record_handoff(
            &log,
            Node::Review("rev-3".into()),
            Some(ChangeId::Commit(head.clone())),
            &wt,
            Info::default(),
            None,
        )
        .unwrap();
        assert_eq!(source.source.produced_by.as_deref(), Some("run-2"));
        let set = replay(&log).unwrap();
        assert!(
            set.live(&run("run-2"), &commit_node(&head), Rel::Produced)
                .is_some()
        );
    }

    #[test]
    fn a_hand_off_decides_its_source_under_the_lock() {
        let tmp = Tmp::new("handoff-race");
        let main = tmp.path().join("app");
        let start = repo_at(&main);
        let (a, b) = (tmp.path().join("wt/a"), tmp.path().join("wt/b"));
        worktree(&main, &a, "a");
        worktree(&main, &b, "b");
        fs::write(a.join("a.txt"), "c\n").unwrap();
        let c = commit(&a, "c");
        // Both worktrees at C, which neither run started on.
        sh(&b, &["reset", "-q", "--hard", &c]);
        let log = log_in(&tmp);
        let mut edits = Vec::new();
        for (r, t, dir, branch) in [("run-a", "t-a", &a, "a"), ("run-b", "t-b", &b, "b")] {
            edits.push(auto(Node::Task(task(t)), run(r), Rel::Ran));
            edits.push(observe(
                run(r),
                Info {
                    workdir: Some(dir.clone()),
                    branch: Some(branch.into()),
                    head: Some(start.clone()),
                    ..Info::default()
                },
            ));
        }
        append(&log, &edits, 1).unwrap();
        let change = ChangeId::Commit(c.clone());
        // The verification in b is recorded while the review in a has read
        // its work tree but not yet taken the lock.
        let review = handoff(
            &log,
            Node::Review("rev-1".into()),
            Some(change.clone()),
            &a,
            Info::default(),
            None,
            || {
                let verify = record_handoff(
                    &log,
                    Node::Verify("ver-1".into()),
                    Some(change.clone()),
                    &b,
                    Info::default(),
                    None,
                )
                .unwrap();
                assert_eq!(verify.source.produced_by.as_deref(), Some("run-b"));
            },
        )
        .unwrap();
        // As one after the other: the review finds run-b by rule 1.
        assert_eq!(
            review.source,
            Source {
                runs: vec!["run-b".into()],
                tasks: vec![task("t-b")],
                produced_by: None,
            }
        );
        let set = replay(&log).unwrap();
        let producers: Vec<&Node> = set
            .links_to(&commit_node(&c), Rel::Produced)
            .map(|l| &l.from)
            .collect();
        assert_eq!(producers, [&run("run-b")]);
    }

    #[test]
    fn a_record_is_judged_on_one_change_only() {
        let tmp = Tmp::new("judged-once");
        let log = log_in(&tmp);
        let rev = Node::Review("rev-1".into());
        let (a, b) = (commit_node(&sha(1)), commit_node(&sha(2)));
        let first =
            linked(&append(&log, &[auto(rev.clone(), a.clone(), Rel::Judged)], 1).unwrap()[0]);
        // Another change, automatic or by hand: refused, never added.
        assert_eq!(
            append(
                &log,
                &[
                    auto(rev.clone(), b.clone(), Rel::Judged),
                    manual(rev.clone(), b.clone(), Rel::Judged),
                ],
                2
            )
            .unwrap(),
            [
                Outcome::Refused(JUDGED_ELSEWHERE.to_owned()),
                Outcome::Refused(JUDGED_ELSEWHERE.to_owned())
            ]
        );
        // The same change again: skipped as before.
        assert_eq!(
            append(&log, &[auto(rev.clone(), a.clone(), Rel::Judged)], 3).unwrap(),
            [Outcome::Skipped]
        );
        // A hand-off of the record on another change writes nothing.
        let lines = fs::read_to_string(&log).unwrap().lines().count();
        let err = record_handoff(
            &log,
            rev.clone(),
            Some(ChangeId::Commit(sha(2))),
            tmp.path(),
            Info {
                label: Some("x".into()),
                ..Info::default()
            },
            None,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(err.to_string(), JUDGED_ELSEWHERE);
        assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), lines);
        // A second Judged written by hand is skipped on read.
        let mut file = OpenOptions::new().append(true).open(&log).unwrap();
        let line = serde_json::to_string(&LinkEvent::Linked {
            id: LinkId::new("lnk-99"),
            from: rev.clone(),
            to: b.clone(),
            rel: Rel::Judged,
            origin: Origin::Auto,
            at: 4,
        })
        .unwrap();
        writeln!(file, "{line}").unwrap();
        drop(file);
        let set = replay(&log).unwrap();
        assert_eq!(set.links_from(&rev, Rel::Judged).count(), 1);
        assert_eq!(set.judged(&rev), Some(&ChangeId::Commit(sha(1))));
        assert_eq!(backlinks(&set, &rev).changes.len(), 1);
        // Once unlinked, another change may be judged.
        append(&log, &[Edit::Unlink(first)], 5).unwrap();
        linked(&append(&log, &[manual(rev.clone(), b, Rel::Judged)], 6).unwrap()[0]);
        assert_eq!(
            replay(&log).unwrap().judged(&rev),
            Some(&ChangeId::Commit(sha(2)))
        );
    }

    #[test]
    fn removing_a_row_from_a_task_never_removes_a_shared_judged_link() {
        let tmp = Tmp::new("rows");
        let log = log_in(&tmp);
        let (t1, t2) = (Node::Task(task("t-1")), Node::Task(task("t-2")));
        let c = commit_node(&sha(3));
        let rev = Node::Review("rev-1".into());
        let outcomes = append(
            &log,
            &[
                auto(t1.clone(), run("run-1"), Rel::Ran),
                auto(t2.clone(), run("run-9"), Rel::Ran),
                auto(run("run-1"), c.clone(), Rel::Produced),
                auto(run("run-9"), c.clone(), Rel::Produced),
                auto(rev.clone(), c.clone(), Rel::Judged),
                // Added by hand on t-1 only.
                manual(rev.clone(), t1.clone(), Rel::About),
            ],
            1,
        )
        .unwrap();
        let judged = linked(&outcomes[4]);
        let about = linked(&outcomes[5]);
        let set = replay(&log).unwrap();
        // t-1: under the change, carrying its own About link, not the
        // shared Judged; not listed again as loose.
        let one = related(&set, &task("t-1"), None);
        let row = &one.runs[0].changes[0].results[0];
        assert_eq!(row.node, rev);
        assert_eq!(row.links, std::slice::from_ref(&about));
        assert!(one.loose.is_empty(), "{:?}", one.loose);
        // t-2 lists it for the shared Judged only: nothing to remove.
        let two = related(&set, &task("t-2"), None);
        assert!(two.runs[0].changes[0].results[0].links.is_empty());
        // Removing t-1's row unlinks its About; the Judged link stays, and
        // t-2 still lists the review.
        let edits: Vec<Edit> = row.links.iter().cloned().map(Edit::Unlink).collect();
        assert_eq!(append(&log, &edits, 2).unwrap(), [Outcome::Unlinked]);
        let set = replay(&log).unwrap();
        assert!(set.link(&judged).is_some());
        assert!(set.link(&about).is_none());
        let two = related(&set, &task("t-2"), None);
        assert_eq!(two.runs[0].changes[0].results[0].node, rev);
        let one = related(&set, &task("t-1"), None);
        assert!(one.runs[0].changes[0].results[0].links.is_empty());
        // About links to the task and to its run both put a loose row
        // there; the row carries only the one to the task.
        let ver = Node::Verify("ver-1".into());
        let outcomes = append(
            &log,
            &[
                auto(ver.clone(), t1.clone(), Rel::About),
                auto(ver.clone(), run("run-1"), Rel::About),
            ],
            3,
        )
        .unwrap();
        let set = replay(&log).unwrap();
        let one = related(&set, &task("t-1"), None);
        assert_eq!(one.loose[0].node, ver);
        assert_eq!(one.loose[0].links, [linked(&outcomes[0])]);
    }

    #[test]
    fn the_current_change_is_listed_under_the_run_it_was_taken_from() {
        let tmp = Tmp::new("current-run");
        let main = tmp.path().join("app");
        repo_at(&main);
        let wt = tmp.path().join("app-wt/task-run-1");
        worktree(&main, &wt, "task-run-1");
        fs::write(wt.join("a.txt"), "v1\n").unwrap();
        let v1 = commit(&wt, "v1");
        let t = Node::Task(task("t-1"));
        let log = log_in(&tmp);
        append(
            &log,
            &[
                auto(t.clone(), run("run-1"), Rel::Ran),
                auto(t.clone(), run("run-2"), Rel::Ran),
                auto(run("run-2"), commit_node(&sha(2)), Rel::Produced),
                observe(
                    run("run-1"),
                    Info {
                        workdir: Some(wt.clone()),
                        ..Info::default()
                    },
                ),
                // run-2's worktree is gone.
                observe(
                    run("run-2"),
                    Info {
                        workdir: Some(tmp.path().join("app-wt/task-run-2")),
                        ..Info::default()
                    },
                ),
            ],
            1,
        )
        .unwrap();
        let set = replay(&log).unwrap();
        let now = task_current(&set, &task("t-1"), &main).unwrap();
        assert_eq!(now, at("run-1", &ChangeId::Commit(v1.clone())));
        let related = related(&set, &task("t-1"), Some(&now));
        let runs: Vec<&str> = related.runs.iter().map(|r| r.run.as_str()).collect();
        assert_eq!(runs, ["run-1", "run-2"]);
        assert_eq!(
            related.runs[0].changes,
            [ChangeEntry {
                change: ChangeId::Commit(v1),
                link: None,
                current: true,
                results: Vec::new(),
            }]
        );
        assert_eq!(related.runs[1].changes.len(), 1);
        assert!(!related.runs[1].changes[0].current);
        // A run that is none of the task's: under the task itself.
        let elsewhere = super::related(
            &set,
            &task("t-1"),
            Some(&at("run-7", &ChangeId::Commit(sha(9)))),
        );
        assert!(elsewhere.changes[0].current);
    }

    #[test]
    fn a_snapshot_is_found_while_any_of_its_capture_dirs_is() {
        let tmp = Tmp::new("captures");
        let log = log_in(&tmp);
        let snap = ChangeId::Snapshot(format!("snap-{}-00000000000000ff", sha(7)));
        let node = Node::Change(snap.clone());
        let review = tmp.path().join("wt/.noa/reviews/rev-1/target");
        let verify = tmp.path().join("wt/.noa/verify/ver-1/target");
        fs::create_dir_all(&review).unwrap();
        fs::create_dir_all(&verify).unwrap();
        for dir in [&review, &verify] {
            append(
                &log,
                &[observe(
                    node.clone(),
                    Info {
                        record: Some(dir.clone()),
                        ..Info::default()
                    },
                )],
                1,
            )
            .unwrap();
        }
        let set = replay(&log).unwrap();
        assert_eq!(set.info_of(&node).record.as_ref(), Some(&verify));
        let data = tmp.path().join("data");
        // The latest capture removed: the review's still holds it.
        fs::remove_dir_all(&verify).unwrap();
        assert_eq!(presence(&set, &node, &data, tmp.path()), Presence::Found);
        fs::remove_dir_all(&review).unwrap();
        assert_eq!(
            presence(&set, &node, &data, tmp.path()),
            Presence::Missing {
                what: format!(
                    "{} {} {}",
                    snap.as_str(),
                    review.display(),
                    verify.display()
                )
            }
        );
    }

    #[test]
    fn change_ids_without_their_form_are_skipped_on_read_and_refused_on_write() {
        let tmp = Tmp::new("ids");
        let log = log_in(&tmp);
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        let rev = Node::Review("rev-1".into());
        let bad = [
            ChangeId::Commit("ああああああああ".into()),
            ChangeId::Commit(sha(10).to_uppercase()),
            ChangeId::Commit(sha(1)[..39].to_owned()),
            ChangeId::Snapshot(format!("snap-{}-ff", sha(1))),
            ChangeId::Snapshot(format!("snap-{}-あああああ", sha(1))),
            ChangeId::Snapshot("snap-x".into()),
        ];
        let mut lines = String::new();
        for (n, change) in bad.iter().enumerate() {
            for event in [
                LinkEvent::Linked {
                    id: LinkId::new(format!("lnk-{n}")),
                    from: run("run-1"),
                    to: Node::Change(change.clone()),
                    rel: Rel::Produced,
                    origin: Origin::Manual,
                    at: 1,
                },
                LinkEvent::Observed {
                    node: Node::Change(change.clone()),
                    info: Info {
                        record: Some(tmp.path().to_path_buf()),
                        ..Info::default()
                    },
                    at: 1,
                },
            ] {
                lines.push_str(&serde_json::to_string(&event).unwrap());
                lines.push('\n');
            }
            // Drawn without a panic, whatever it holds.
            let _ = change.short();
            assert!(!change.is_valid(), "{change:?}");
        }
        fs::write(&log, lines).unwrap();
        let set = replay(&log).unwrap();
        assert!(set.links.is_empty());
        assert!(set.info.is_empty());
        assert!(set.captures.is_empty());
        assert_eq!(
            ChangeId::Commit("ああああああああ".into()).short(),
            "あああああああ"
        );
        // Written through append: refused.
        let outcomes = append(
            &log,
            &[
                auto(rev.clone(), Node::Change(bad[0].clone()), Rel::Judged),
                observe(Node::Change(bad[3].clone()), Info::default()),
            ],
            2,
        )
        .unwrap();
        assert!(
            outcomes.iter().all(|o| matches!(o, Outcome::Refused(_))),
            "{outcomes:?}"
        );
        // Both forms of a good id pass.
        for good in [
            ChangeId::Commit(sha(1)),
            ChangeId::Commit(format!("{}{}", sha(1), &sha(2)[..24])),
            ChangeId::Snapshot(format!("snap-{}-0123456789abcdef", sha(1))),
        ] {
            assert!(good.is_valid(), "{good:?}");
        }
    }

    #[test]
    fn a_tasks_rows_carry_only_links_at_the_task_itself() {
        let tmp = Tmp::new("own-links");
        let log = log_in(&tmp);
        let (a, b) = (Node::Task(task("t-a")), Node::Task(task("t-b")));
        let c = commit_node(&sha(3));
        let (rev, ver) = (Node::Review("rev-1".into()), Node::Verify("ver-1".into()));
        let outcomes = append(
            &log,
            &[
                // run-1 is shared by t-a and t-b.
                auto(a.clone(), run("run-1"), Rel::Ran),
                auto(b.clone(), run("run-1"), Rel::Ran),
                auto(run("run-1"), c.clone(), Rel::Produced),
                // A review About the shared run only, and a verification
                // of its change About the run and t-a.
                auto(rev.clone(), run("run-1"), Rel::About),
                auto(ver.clone(), c, Rel::Judged),
                auto(ver.clone(), run("run-1"), Rel::About),
                auto(ver.clone(), a, Rel::About),
                manual(Node::Task(task("t-a")), commit_node(&sha(4)), Rel::Produced),
            ],
            1,
        )
        .unwrap();
        let set = replay(&log).unwrap();
        let one = related(&set, &task("t-a"), None);
        assert_eq!(one.runs[0].link, linked(&outcomes[0]));
        // The run's Produced link is the run's: not on t-a's row.
        assert_eq!(one.runs[0].changes[0].link, None);
        let result = &one.runs[0].changes[0].results[0];
        assert_eq!(result.node, ver);
        assert_eq!(result.links, [linked(&outcomes[6])]);
        // About the shared run alone: listed, nothing to remove.
        assert_eq!(one.loose[0].node, rev);
        assert!(one.loose[0].links.is_empty());
        assert_eq!(one.changes[0].link, Some(linked(&outcomes[7])));
        // Removing every link t-a's rows carry leaves t-b's view as it was.
        let before = related(&set, &task("t-b"), None);
        let carried: Vec<Edit> = one
            .runs
            .iter()
            .map(|r| Some(r.link.clone()))
            .chain(
                one.runs
                    .iter()
                    .flat_map(|r| &r.changes)
                    .map(|c| c.link.clone()),
            )
            .chain(one.changes.iter().map(|c| c.link.clone()))
            .flatten()
            .chain(
                one.runs
                    .iter()
                    .flat_map(|r| &r.changes)
                    .chain(&one.changes)
                    .flat_map(|c| &c.results)
                    .chain(&one.loose)
                    .flat_map(|r| r.links.clone()),
            )
            .map(Edit::Unlink)
            .collect();
        assert_eq!(carried.len(), 3, "{carried:?}");
        append(&log, &carried, 2).unwrap();
        let set = replay(&log).unwrap();
        assert_eq!(related(&set, &task("t-b"), None), before);
        assert_eq!(related(&set, &task("t-a"), None), Related::default());
    }

    #[test]
    fn with_its_worktree_gone_a_runs_last_change_is_found_by_commit_topology() {
        let tmp = Tmp::new("topology");
        let main = tmp.path().join("app");
        let start = repo_at(&main);
        let wt = tmp.path().join("app-wt/task-run-1");
        worktree(&main, &wt, "task-run-1");
        fs::write(wt.join("a.txt"), "v1\n").unwrap();
        let v1 = commit(&wt, "v1");
        fs::write(wt.join("a.txt"), "v2\n").unwrap();
        let v2 = commit(&wt, "v2");
        let log = log_in(&tmp);
        // v2 recorded first (a hand-off), v1 after it (the rev-list pass,
        // which skips v2).
        append(
            &log,
            &[
                auto(Node::Task(task("t-1")), run("run-1"), Rel::Ran),
                observe(
                    run("run-1"),
                    Info {
                        workdir: Some(wt.clone()),
                        branch: Some("task-run-1".into()),
                        head: Some(start),
                        ..Info::default()
                    },
                ),
                auto(run("run-1"), commit_node(&v2), Rel::Produced),
                auto(run("run-1"), commit_node(&v1), Rel::Produced),
            ],
            1,
        )
        .unwrap();
        sh(
            &main,
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
        );
        let current = || task_current(&replay(&log).unwrap(), &task("t-1"), &main);
        assert_eq!(current(), Some(at("run-1", &ChangeId::Commit(v2.clone()))));
        // A snapshot on v1, recorded last, is older than v2.
        let on = |head: &str, hex: &str| ChangeId::Snapshot(format!("snap-{head}-{hex}"));
        let produce = |change: &ChangeId| {
            append(
                &log,
                &[auto(
                    run("run-1"),
                    Node::Change(change.clone()),
                    Rel::Produced,
                )],
                2,
            )
            .unwrap();
        };
        produce(&on(&v1, "00000000000000aa"));
        assert_eq!(current(), Some(at("run-1", &ChangeId::Commit(v2.clone()))));
        // A snapshot on v2 recorded after v2 is the work on it since.
        produce(&on(&v2, "00000000000000bb"));
        assert_eq!(current(), Some(at("run-1", &on(&v2, "00000000000000bb"))));
        // History that split (a commit on the start, not on v2): neither
        // tip descends from the other, so the latest recorded one.
        fs::write(main.join("b.txt"), "side\n").unwrap();
        let side = ChangeId::Commit(commit(&main, "side"));
        produce(&side);
        assert_eq!(current(), Some(at("run-1", &side)));
    }

    #[test]
    fn work_restored_to_a_commit_after_its_snapshot_leaves_the_commit_current() {
        let tmp = Tmp::new("restored");
        let log = log_in(&tmp);
        let c = sha(3);
        let on_c = |hex: &str| ChangeId::Snapshot(format!("snap-{c}-{hex}"));
        // Snapshot S on C reviewed, then the work restored to C and C
        // recorded; the worktree is gone.
        append(
            &log,
            &[
                auto(Node::Task(task("t-1")), run("run-1"), Rel::Ran),
                observe(
                    run("run-1"),
                    Info {
                        workdir: Some(tmp.path().join("app-wt/task-run-1")),
                        branch: Some("task-run-1".into()),
                        ..Info::default()
                    },
                ),
                auto(
                    run("run-1"),
                    Node::Change(on_c("00000000000000aa")),
                    Rel::Produced,
                ),
                auto(run("run-1"), commit_node(&c), Rel::Produced),
            ],
            1,
        )
        .unwrap();
        let current = || task_current(&replay(&log).unwrap(), &task("t-1"), tmp.path());
        assert_eq!(current(), Some(at("run-1", &ChangeId::Commit(c.clone()))));
        // New work on C, recorded after C: that snapshot.
        let later = on_c("00000000000000bb");
        append(
            &log,
            &[auto(
                run("run-1"),
                Node::Change(later.clone()),
                Rel::Produced,
            )],
            2,
        )
        .unwrap();
        assert_eq!(current(), Some(at("run-1", &later)));
    }

    #[test]
    fn a_snapshot_without_a_capture_is_missing_once_its_work_tree_is_gone() {
        let tmp = Tmp::new("snap-tree");
        let log = log_in(&tmp);
        let data = tmp.path().join("data");
        let wt = tmp.path().join("wt");
        fs::create_dir_all(&wt).unwrap();
        let id = |hex: &str| format!("snap-{}-{hex}", sha(7));
        let node = |id: &str| Node::Change(ChangeId::Snapshot(id.into()));
        let (own, by_run, bare) = (
            id("00000000000000aa"),
            id("00000000000000bb"),
            id("00000000000000cc"),
        );
        append(
            &log,
            &[
                // Its own work tree recorded, no capture dir.
                observe(
                    node(&own),
                    Info {
                        workdir: Some(wt.clone()),
                        head: Some(sha(7)),
                        ..Info::default()
                    },
                ),
                // Only produced by a run: the run's work tree is its.
                observe(
                    run("run-1"),
                    Info {
                        workdir: Some(wt.clone()),
                        ..Info::default()
                    },
                ),
                auto(run("run-1"), node(&by_run), Rel::Produced),
                // No work tree recorded anywhere.
                auto(run("run-2"), node(&bare), Rel::Produced),
            ],
            1,
        )
        .unwrap();
        let set = replay(&log).unwrap();
        let of = |id: &str| presence(&set, &node(id), &data, tmp.path());
        // The tree is there: whether it is still at the state is not known.
        for id in [&own, &by_run, &bare] {
            assert_eq!(of(id), Presence::Unknown, "{id}");
        }
        fs::remove_dir_all(&wt).unwrap();
        for id in [&own, &by_run] {
            assert_eq!(
                of(id),
                Presence::Missing {
                    what: format!("{id} {}", wt.display())
                }
            );
        }
        assert_eq!(of(&bare), Presence::Unknown);
    }

    #[test]
    fn a_local_task_id_with_two_dots_in_it_is_looked_up_in_the_tasks_folder() {
        let tmp = Tmp::new("task-dots");
        let repo = tmp.path();
        let data = repo.join("data");
        let set = LinkSet::default();
        for id in ["t-review-v1..v2", "t-.."] {
            let file = repo.join(".noa/tasks").join(format!("{id}.md"));
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, "# x\n").unwrap();
            assert_eq!(
                presence(&set, &Node::Task(task(id)), &data, repo),
                Presence::Found,
                "{id:?}"
            );
        }
        // An id that is exactly `.` or `..` never is one.
        for bad in [".", ".."] {
            assert_eq!(
                presence(&set, &Node::Task(task(bad)), &data, repo),
                Presence::Missing { what: bad.into() },
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_current_change_lists_every_result_that_judged_it() {
        let t = Node::Task(task("t-1"));
        let head = ChangeId::Commit(sha(5));
        let link = |id: &str, from: Node, to: Node, rel: Rel| LinkEvent::Linked {
            id: LinkId::new(id),
            from,
            to,
            rel,
            origin: Origin::Auto,
            at: 1,
        };
        // A run in the repository itself at its existing HEAD: nothing
        // Produced; a review judged that HEAD with no About, and a
        // verification judged it About the task.
        let set = fold(&[
            link("lnk-1", t.clone(), run("run-1"), Rel::Ran),
            link(
                "lnk-2",
                Node::Review("rev-1".into()),
                Node::Change(head.clone()),
                Rel::Judged,
            ),
            link(
                "lnk-3",
                Node::Verify("ver-1".into()),
                Node::Change(head.clone()),
                Rel::Judged,
            ),
            link("lnk-4", Node::Verify("ver-1".into()), t, Rel::About),
        ]);
        let related = related(&set, &task("t-1"), Some(&at("run-1", &head)));
        let rows = &related.runs[0].changes;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].change, head);
        assert!(rows[0].current);
        assert_eq!(rows[0].link, None);
        let results: Vec<(&Node, bool, &[LinkId])> = rows[0]
            .results
            .iter()
            .map(|r| (&r.node, r.stale, r.links.as_slice()))
            .collect();
        assert_eq!(
            results,
            [
                (&Node::Review("rev-1".into()), false, &[][..]),
                (
                    &Node::Verify("ver-1".into()),
                    false,
                    &[LinkId::new("lnk-4")][..]
                ),
            ]
        );
        // Listed under the change, not again as loose.
        assert!(related.loose.is_empty(), "{:?}", related.loose);
    }

    #[test]
    fn a_newer_run_at_a_change_an_older_run_produced_gets_the_current_row() {
        let tmp = Tmp::new("current-newer");
        let main = tmp.path().join("app");
        let c = repo_at(&main);
        let wt = tmp.path().join("app-wt/task-run-2");
        worktree(&main, &wt, "task-run-2");
        let t = Node::Task(task("t-1"));
        let change = ChangeId::Commit(c.clone());
        let log = log_in(&tmp);
        append(
            &log,
            &[
                auto(t.clone(), run("run-1"), Rel::Ran),
                auto(t.clone(), run("run-2"), Rel::Ran),
                auto(run("run-1"), commit_node(&c), Rel::Produced),
                auto(Node::Review("rev-1".into()), commit_node(&c), Rel::Judged),
                observe(
                    run("run-2"),
                    Info {
                        workdir: Some(wt.clone()),
                        branch: Some("task-run-2".into()),
                        head: Some(c.clone()),
                        ..Info::default()
                    },
                ),
            ],
            1,
        )
        .unwrap();
        let set = replay(&log).unwrap();
        // run-2's worktree sits at C, which run-1 produced.
        let now = task_current(&set, &task("t-1"), &main).unwrap();
        assert_eq!(now, at("run-2", &change));
        let related = related(&set, &task("t-1"), Some(&now));
        let runs: Vec<&str> = related.runs.iter().map(|r| r.run.as_str()).collect();
        assert_eq!(runs, ["run-1", "run-2"]);
        // run-1 keeps C, not marked; its result judged the current change,
        // so it is not stale.
        let older = &related.runs[0].changes;
        assert_eq!(older.len(), 1);
        assert_eq!((&older[0].change, older[0].current), (&change, false));
        assert!(!older[0].results[0].stale);
        // run-2 gets the current row, with the result that judged C.
        let newer = &related.runs[1].changes;
        assert_eq!(newer.len(), 1);
        assert_eq!((&newer[0].change, newer[0].current), (&change, true));
        assert_eq!(newer[0].results[0].node, Node::Review("rev-1".into()));
        assert!(!newer[0].results[0].stale);
        // Recorded under both: only run-2's row is marked.
        append(
            &log,
            &[auto(run("run-2"), commit_node(&c), Rel::Produced)],
            2,
        )
        .unwrap();
        let set = replay(&log).unwrap();
        let related = super::related(&set, &task("t-1"), Some(&now));
        let marks: Vec<(usize, bool)> = related
            .runs
            .iter()
            .map(|r| (r.changes.len(), r.changes[0].current))
            .collect();
        assert_eq!(marks, [(1, false), (1, true)]);
    }

    #[test]
    fn an_explicit_commit_at_head_finds_its_run_when_the_tree_has_no_snapshot_id() {
        let tmp = Tmp::new("handoff-no-snap");
        let (log, wt, _) = run_in_worktree(&tmp);
        fs::write(wt.join("a.txt"), "v1\n").unwrap();
        let v1 = commit(&wt, "v1");
        // Untracked work over the cap: the work tree has no snapshot id.
        let big = vec![b'x'; SNAPSHOT_MAX_BYTES as usize + 1];
        fs::write(wt.join("big.bin"), big).unwrap();
        assert_eq!(current_change(&wt), Err(TOO_LARGE.to_owned()));
        let handoff = record_handoff(
            &log,
            Node::Review("rev-1".into()),
            Some(ChangeId::Commit(v1.clone())),
            &wt,
            Info::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            handoff.source,
            Source {
                runs: vec!["run-2".into()],
                tasks: vec![task("t-1")],
                produced_by: Some("run-2".into()),
            }
        );
        assert_eq!(handoff.notice(), None);
        let set = replay(&log).unwrap();
        assert!(
            set.live(&run("run-2"), &commit_node(&v1), Rel::Produced)
                .is_some()
        );
    }

    #[test]
    fn a_stash_header_does_not_make_a_clean_tree_dirty() {
        let tmp = Tmp::new("snap-stash");
        let repo = tmp.path().join("app");
        let head = repo_at(&repo);
        fs::write(repo.join("a.txt"), "stashed\n").unwrap();
        sh(&repo, &["stash", "push", "-q"]);
        sh(&repo, &["config", "status.showStash", "true"]);
        sh(&repo, &["config", "status.branch", "true"]);
        assert_eq!(snapshot_id(&repo).unwrap(), ChangeId::Commit(head));
        // Header lines never count; an entry after one does.
        assert!(!dirty(b"# stash 1\0# branch.oid x\0"));
        assert!(dirty(b"# stash 1\0? b.txt\0"));
        fs::write(repo.join("b.txt"), "b\n").unwrap();
        assert!(matches!(snapshot_id(&repo), Ok(ChangeId::Snapshot(_))));
    }

    #[test]
    fn a_local_task_id_tasks_accepts_is_looked_up_in_the_tasks_folder() {
        let tmp = Tmp::new("task-ids");
        let repo = tmp.path();
        let data = repo.join("data");
        let set = LinkSet::default();
        let named = Node::Task(task("t-fix-ui"));
        let file = repo.join(".noa/tasks/t-fix-ui.md");
        assert_eq!(
            presence(&set, &named, &data, repo),
            Presence::Missing {
                what: format!("t-fix-ui {}", file.display())
            }
        );
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "# fix\n").unwrap();
        assert_eq!(presence(&set, &named, &data, repo), Presence::Found);
        // Never a path out of the folder, never an id Tasks refuses.
        for bad in ["t-a/b", "t-../x", "t-", "x-1", "t-a\0b"] {
            assert_eq!(
                presence(&set, &Node::Task(task(bad)), &data, repo),
                Presence::Missing { what: bad.into() },
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_hand_off_returns_what_the_store_refused() {
        let tmp = Tmp::new("handoff-refused");
        let log = log_in(&tmp);
        // A short SHA: the Judged link is refused, the rest is written.
        let handoff = record_handoff(
            &log,
            Node::Review("rev-1".into()),
            Some(ChangeId::Commit("abc1234".into())),
            tmp.path(),
            Info {
                label: Some("x".into()),
                ..Info::default()
            },
            None,
        )
        .unwrap();
        let outcomes: Vec<&Outcome> = handoff.outcomes.iter().map(|(_, o)| o).collect();
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        assert_eq!(outcomes[0], &Outcome::Observed);
        assert!(matches!(
            &handoff.outcomes[1],
            (
                Edit::Link {
                    rel: Rel::Judged,
                    ..
                },
                Outcome::Refused(_)
            )
        ));
        let notice = handoff.notice().unwrap();
        assert!(
            notice.contains("レビュー rev-1 → 版 abc1234 (Judged)"),
            "{notice}"
        );
        assert!(notice.contains("版の ID の形ではありません"), "{notice}");
        assert!(replay(&log).unwrap().links.is_empty());
        // Everything recorded, or recorded before: no notice.
        let full = Some(ChangeId::Commit(sha(1)));
        for _ in 0..2 {
            let handoff = record_handoff(
                &log,
                Node::Review("rev-2".into()),
                full.clone(),
                tmp.path(),
                Info::default(),
                None,
            )
            .unwrap();
            assert_eq!(handoff.notice(), None, "{:?}", handoff.outcomes);
        }
    }

    #[test]
    fn a_hand_off_resolves_work_tree_paths_before_taking_the_lock() {
        let tmp = Tmp::new("handoff-paths");
        let main = tmp.path().join("app");
        let start = repo_at(&main);
        let wt = tmp.path().join("real/task-run-2");
        worktree(&main, &wt, "task-run-2");
        fs::write(wt.join("a.txt"), "v1\n").unwrap();
        let v1 = commit(&wt, "v1");
        // The run's work tree recorded through a link to its parent.
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(tmp.path().join("real"), &alias).unwrap();
        let log = log_in(&tmp);
        append(
            &log,
            &[
                auto(Node::Task(task("t-1")), run("run-2"), Rel::Ran),
                observe(
                    run("run-2"),
                    Info {
                        workdir: Some(alias.join("task-run-2")),
                        branch: Some("task-run-2".into()),
                        head: Some(start),
                        ..Info::default()
                    },
                ),
            ],
            1,
        )
        .unwrap();
        // The link is gone by the time the lock is taken: a lookup under
        // the lock could no longer resolve it.
        let handoff = handoff(
            &log,
            Node::Review("rev-1".into()),
            Some(ChangeId::Commit(v1)),
            &wt,
            Info::default(),
            None,
            || fs::remove_file(&alias).unwrap(),
        )
        .unwrap();
        assert_eq!(handoff.source.produced_by.as_deref(), Some("run-2"));
        assert_eq!(handoff.source.tasks, [task("t-1")]);
    }

    #[test]
    fn json_shapes_are_fixed() {
        let cases = [
            (task("t-1"), r#"{"local":"t-1"}"#),
            (
                TaskKey::GitHub {
                    repo: "o/n".into(),
                    number: 12,
                },
                r#"{"github":{"repo":"o/n","number":12}}"#,
            ),
            (
                TaskKey::Code {
                    path: "a.rs".into(),
                    key: "k".into(),
                },
                r#"{"code":{"path":"a.rs","key":"k"}}"#,
            ),
        ];
        for (key, json) in cases {
            assert_eq!(serde_json::to_string(&key).unwrap(), json);
            assert_eq!(serde_json::from_str::<TaskKey>(json).unwrap(), key);
        }
        let event = LinkEvent::Linked {
            id: LinkId::new("lnk-1"),
            from: Node::Review("rev-1".into()),
            to: Node::Change(ChangeId::Snapshot("snap-x".into())),
            rel: Rel::Judged,
            origin: Origin::Auto,
            at: 9,
        };
        let line = serde_json::to_string(&event).unwrap();
        assert_eq!(
            line,
            r#"{"event":"Linked","id":"lnk-1","from":{"review":"rev-1"},"to":{"change":{"snapshot":"snap-x"}},"rel":"judged","origin":"auto","at":9}"#
        );
        assert_eq!(serde_json::from_str::<LinkEvent>(&line).unwrap(), event);
        let observed = LinkEvent::Observed {
            node: Node::Run("run-1".into()),
            info: Info {
                branch: Some("task-run-1".into()),
                ..Info::default()
            },
            at: 2,
        };
        let line = serde_json::to_string(&observed).unwrap();
        assert_eq!(
            line,
            r#"{"event":"Observed","node":{"run":"run-1"},"info":{"branch":"task-run-1"},"at":2}"#
        );
        assert_eq!(serde_json::from_str::<LinkEvent>(&line).unwrap(), observed);
    }

    #[test]
    fn change_ids_are_named_short() {
        let head = sha(10);
        let snap = ChangeId::Snapshot(format!("snap-{head}-0123456789abcdef"));
        assert_eq!(snap.short(), "aaaaaaa + 未コミット (snap 01234567)");
        assert_eq!(snap.head(), Some(head.as_str()));
        assert_eq!(ChangeId::Commit(head.clone()).short(), "aaaaaaa");
        assert_eq!(ChangeId::Snapshot("odd".into()).head(), None);
    }

    #[test]
    fn the_three_copies_of_this_file_are_byte_equal() {
        let widgets = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let read = |w: &str| fs::read(widgets.join(w).join("src/links.rs")).unwrap();
        let tasks = read("tasks");
        assert!(
            tasks == read("review"),
            "widgets/review/src/links.rs differs from tasks'"
        );
        assert!(
            tasks == read("verify"),
            "widgets/verify/src/links.rs differs from tasks'"
        );
    }
}
