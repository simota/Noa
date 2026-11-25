//! Where the review on screen came from (artifact-links L1-5): its task
//! and the exact change it judged, read from the repository's link store
//! when the review is shown and again at most once a second while it stays
//! shown (only when the store file changed), for the target card's タスク
//! and 対象の版 rows.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use noa_gpui_core::widgets::Tone;
use review::links::{self, BackLinks, LinkSet, Nav, Node, Presence, TaskKey};
use review::model::types::Session;

use crate::session::runner::handoff;

/// The shortest time between two reads of the link store.
pub(super) const REREAD: Duration = Duration::from_secs(1);

/// How often the records the links point at are looked for again while
/// the store itself is unchanged (a task file or capture removed or
/// restored meanwhile).
pub(super) const RECHECK: Duration = Duration::from_secs(5);

/// What the card says when the review has no task.
pub(super) const NO_TASK: &str = "関連するタスクはありません";

/// One row of the target card: `<head> <badge> <text>`, then either the
/// button (its label and where it goes) or why the record is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
    pub head: &'static str,
    pub badge: Option<(String, Tone)>,
    pub text: String,
    /// 記録が見つからない（…）: shown in place of the button.
    pub missing: Option<String>,
    pub open: Option<(&'static str, Nav)>,
}

/// The rows: one per task (none: [`NO_TASK`]), and the change judged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Rows {
    pub tasks: Vec<Row>,
    pub change: Option<Row>,
}

/// The rows of `back`, with `presence` telling whether each record is
/// still there and `repo` (the main worktree) where Tasks opens. Pure.
pub(super) fn rows(
    back: &BackLinks,
    presence: &dyn Fn(&Node) -> Presence,
    repo: Option<&Path>,
) -> Rows {
    let open_task = |task: &TaskKey| repo.map(|repo| Nav::task(repo, task));
    let tasks = back
        .tasks
        .iter()
        .map(|t| {
            let missing = missing(presence(&Node::Task(t.task.clone())));
            Row {
                head: "タスク:",
                badge: Some(badge(&t.task)),
                text: t.info.label.clone().unwrap_or_else(|| t.task.select()),
                open: if missing.is_none() {
                    open_task(&t.task).map(|nav| ("タスクを開く", nav))
                } else {
                    None
                },
                missing,
            }
        })
        .collect();
    let change = back.changes.first().map(|c| {
        let missing = missing(presence(&Node::Change(c.change.clone())));
        // Its first task still there lists it in 関連成果; with none, the
        // id is copied.
        let nav = c
            .tasks
            .iter()
            .find(|t| {
                !matches!(
                    presence(&Node::Task((*t).clone())),
                    Presence::Missing { .. }
                )
            })
            .and_then(open_task)
            .unwrap_or_else(|| Nav::Copy(c.change.as_str().to_owned()));
        Row {
            head: "対象の版:",
            badge: None,
            text: c.change.short(),
            open: missing.is_none().then_some(("版を開く", nav)),
            missing,
        }
    });
    Rows { tasks, change }
}

/// As Tasks badges a task: ローカル, `#N`, or the marker's file name.
fn badge(task: &TaskKey) -> (String, Tone) {
    match task {
        TaskKey::Local(_) => ("ローカル".to_owned(), Tone::Accent),
        TaskKey::GitHub { number, .. } => (format!("#{number}"), Tone::Neutral),
        TaskKey::Code { path, .. } => (
            path.rsplit('/').next().unwrap_or(path).to_owned(),
            Tone::Warning,
        ),
    }
}

fn missing(presence: Presence) -> Option<String> {
    match presence {
        Presence::Missing { what } => Some(format!("記録が見つからない（{what}）")),
        Presence::Found | Presence::Unknown => None,
    }
}

/// A store file's length and modification time: it is read again only
/// once they change (`None`: not there).
type Stamp = Option<(u64, Option<SystemTime>)>;

fn stamp(log: &Path) -> Stamp {
    let meta = fs::metadata(log).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

/// Where a shown review's links are: its store and its repository's main
/// worktree (`None`: not known, for a store found by the review's links).
struct Place {
    log: PathBuf,
    repo: Option<PathBuf>,
}

/// The back-links of the review on screen.
pub(super) struct Back {
    /// The review they are of (its store directory).
    dir: PathBuf,
    record: Node,
    /// `<data dir>/noa`.
    data: Option<PathBuf>,
    place: Option<Place>,
    /// The stamp of each store file as last read: the review's, or while
    /// it has none, each store looked through for its links.
    stamps: BTreeMap<PathBuf, Stamp>,
    read_at: Instant,
    /// The store as last read, and when presence was last looked up.
    set: Option<LinkSet>,
    checked_at: Instant,
    links: BackLinks,
    pub rows: Rows,
}

impl Back {
    /// Reads the links of the review `session`, logged in `dir`
    /// (`<data dir>/noa/reviews/<id>`: the stores are beside the reviews).
    /// Its store is the one recorded at its hand-off, else its work tree's
    /// repository's; with neither, the store that has its links, looked
    /// for again on each read until one does.
    pub(super) fn load(dir: &Path, session: &Session) -> Self {
        let data = dir.parent().and_then(Path::parent).map(Path::to_path_buf);
        let place = handoff::recorded(dir)
            .map(|place| Place {
                log: place.log,
                repo: Some(place.repo),
            })
            .or_else(|| {
                let workdir = session.workdir.as_deref()?;
                let repo = links::main_worktree(workdir).ok()?;
                let log = data
                    .as_ref()?
                    .join("links")
                    .join(links::repo_key(&repo))
                    .join(links::LINKS_FILE);
                Some(Place {
                    log,
                    repo: Some(repo),
                })
            });
        let mut back = Self {
            dir: dir.to_owned(),
            record: Node::Review(session.id.to_string()),
            data,
            place,
            stamps: BTreeMap::new(),
            read_at: Instant::now(),
            set: None,
            checked_at: Instant::now(),
            links: BackLinks::default(),
            rows: Rows::default(),
        };
        back.read();
        back
    }

    /// Whether these are the links of the review logged in `dir`.
    pub(super) fn is_of(&self, dir: &Path) -> bool {
        self.dir == dir
    }

    /// Lets the next [`Back::reread`] read at once (tests only).
    #[cfg(test)]
    pub(super) fn expire(&mut self) {
        self.read_at = Instant::now() - REREAD;
        self.checked_at = Instant::now() - RECHECK;
    }

    /// Reads the store again when [`REREAD`] has passed and it changed;
    /// whether the rows changed.
    pub(super) fn reread(&mut self) -> bool {
        if self.read_at.elapsed() < REREAD {
            return false;
        }
        self.read()
    }

    /// Reads the store when its file changed since the last read; whether
    /// the rows changed. Presence (git, stat) is looked at again when the
    /// links changed, else once [`RECHECK`] has passed.
    fn read(&mut self) -> bool {
        self.read_at = Instant::now();
        let set = match &self.place {
            Some(place) => {
                let log = place.log.clone();
                if !self.changed(&log) {
                    if self.checked_at.elapsed() < RECHECK {
                        return false;
                    }
                    let Some(set) = self.set.take() else {
                        return false;
                    };
                    let changed = self.show(&set);
                    self.set = Some(set);
                    return changed;
                }
                // An unreadable store keeps what was shown.
                let Ok(set) = links::replay(&log) else {
                    return false;
                };
                set
            }
            None => match self.find() {
                Some((place, set)) => {
                    self.place = Some(place);
                    set
                }
                None => return false,
            },
        };
        let changed = self.show(&set);
        self.set = Some(set);
        changed
    }

    /// Whether `log` changed since it was last read; notes it as read.
    fn changed(&mut self, log: &Path) -> bool {
        let now = stamp(log);
        self.stamps.insert(log.to_owned(), now) != Some(now)
    }

    /// The store under `<data dir>/noa/links/*/` that has the review's
    /// links, read from those that changed since the last look.
    fn find(&mut self) -> Option<(Place, LinkSet)> {
        let data = self.data.clone()?;
        let mut logs: Vec<PathBuf> = fs::read_dir(data.join("links"))
            .ok()?
            .filter_map(|entry| Some(entry.ok()?.path().join(links::LINKS_FILE)))
            .collect();
        logs.sort();
        logs.into_iter().find_map(|log| {
            if !self.changed(&log) {
                return None;
            }
            let set = links::replay(&log).ok()?;
            if !set.nodes().contains(&self.record) {
                return None;
            }
            let repo = repo_of(&set, &log, &data);
            Some((Place { log, repo }, set))
        })
    }

    /// Shows the links `set` holds; whether the rows changed.
    fn show(&mut self, set: &LinkSet) -> bool {
        self.checked_at = Instant::now();
        let links = links::backlinks(set, &self.record);
        let repo = self.place.as_ref().and_then(|p| p.repo.as_deref());
        let presence = |node: &Node| match (&self.data, repo) {
            (Some(data), Some(repo)) => links::presence(set, node, data, repo),
            _ => Presence::Unknown,
        };
        let rows = rows(&links, &presence, repo);
        self.links = links;
        let changed = rows != self.rows;
        self.rows = rows;
        changed
    }
}

/// The main worktree of the repository the store `log` is keyed by: the
/// one a work tree or task file named in `set` is in, whose key is the
/// store's directory name. `None` when none of them is still there.
fn repo_of(set: &LinkSet, log: &Path, data: &Path) -> Option<PathBuf> {
    let key = log.parent()?.file_name()?.to_str()?;
    let named: BTreeSet<&Path> = set
        .info
        .iter()
        .flat_map(|(node, info)| {
            let task = matches!(node, Node::Task(_)).then_some(info.record.as_deref());
            [info.workdir.as_deref(), task.flatten()]
        })
        .flatten()
        .filter(|path| !path.starts_with(data))
        .collect();
    named.into_iter().find_map(|path| {
        let there = path.ancestors().find(|dir| dir.is_dir())?;
        let repo = links::main_worktree(there).ok()?;
        (links::repo_key(&repo) == key).then_some(repo)
    })
}
