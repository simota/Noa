//! Where the review on screen came from (artifact-links L1-5): its task
//! and the exact change it judged, read from the repository's link store
//! when the review is shown and again at most once a second while it stays
//! shown, for the target card's タスク and 対象の版 rows.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use noa_gpui_core::widgets::Tone;
use review::links::{self, BackLinks, Nav, Node, Presence, TaskKey};
use review::model::types::Session;

/// The shortest time between two reads of the link store.
pub(super) const REREAD: Duration = Duration::from_secs(1);

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
        // Its task's 関連成果 lists it; with no task, the id is copied.
        let nav = c
            .tasks
            .first()
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

/// Where a shown review's links are: its store, its repository's main
/// worktree and the data dir, found once per review shown.
struct Place {
    log: PathBuf,
    repo: PathBuf,
    data: PathBuf,
}

/// The back-links of the review on screen.
pub(super) struct Back {
    /// The review they are of (its store directory).
    dir: PathBuf,
    record: Node,
    place: Option<Place>,
    read_at: Instant,
    links: BackLinks,
    pub rows: Rows,
}

impl Back {
    /// Reads the links of the review `session`, logged in `dir`
    /// (`<data dir>/noa/reviews/<id>`: the store is beside the reviews).
    pub(super) fn load(dir: &Path, session: &Session) -> Self {
        let place = session.workdir.as_deref().and_then(|workdir| {
            let data = dir.parent()?.parent()?.to_path_buf();
            let repo = links::main_worktree(workdir).ok()?;
            let log = links::path(&data.join("links"), &repo).ok()?;
            Some(Place { log, repo, data })
        });
        let mut back = Self {
            dir: dir.to_owned(),
            record: Node::Review(session.id.to_string()),
            place,
            read_at: Instant::now(),
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
    }

    /// Reads the store again when [`REREAD`] has passed; whether the rows
    /// changed.
    pub(super) fn reread(&mut self) -> bool {
        if self.read_at.elapsed() < REREAD {
            return false;
        }
        self.read()
    }

    /// Reads the store; whether the rows changed. Presence (git, stat) is
    /// looked at only when the links did.
    fn read(&mut self) -> bool {
        self.read_at = Instant::now();
        let Some(place) = &self.place else {
            return false;
        };
        // An unreadable store keeps what was shown.
        let Ok(set) = links::replay(&place.log) else {
            return false;
        };
        let links = links::backlinks(&set, &self.record);
        if links == self.links {
            return false;
        }
        let presence = |node: &Node| links::presence(&set, node, &place.data, &place.repo);
        let rows = rows(&links, &presence, Some(&place.repo));
        self.links = links;
        let changed = rows != self.rows;
        self.rows = rows;
        changed
    }
}
