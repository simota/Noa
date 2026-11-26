//! The view kept across runs: the tab, the grouping, the list's scope, the
//! map's axis, the timeline's scale and the filter. Saved whenever one of
//! them changes and when the window closes, restored at start; the
//! command line's `--tab`, `--group` and `--filter` win over what was
//! saved. The file sits beside Noa's own session state, as the other
//! widgets keep theirs (`<data-dir>/noa/agent-dashboard/view.json`).

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::fleet::{Grouping, Scope, one_line};

use super::Tab;
use super::gantt::TimeScale;
use super::mindmap::MapAxis;

/// The longest filter kept: a query is typed by hand.
const FILTER_MAX_CHARS: usize = 1000;
/// A file larger than this is not ours to read.
const FILE_MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefs {
    pub(super) tab: Tab,
    pub(super) grouping: Grouping,
    pub(super) scope: Scope,
    pub(super) map_axis: MapAxis,
    pub(super) time_scale: TimeScale,
    pub(super) filter: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            tab: Tab::Operations,
            grouping: Grouping::Orchestrator,
            scope: Scope::Current,
            map_axis: MapAxis::Orchestrator,
            time_scale: TimeScale::All,
            filter: String::new(),
        }
    }
}

/// Where the view is kept: `<data-dir>/noa/agent-dashboard/view.json`,
/// beside Noa's session state as `widgets/discussion` keeps its sessions.
pub fn default_path() -> Option<PathBuf> {
    let state = noa_config::session_state_path()?;
    Some(state.parent()?.join("agent-dashboard").join("view.json"))
}

fn tab_name(tab: Tab) -> &'static str {
    match tab {
        Tab::Operations => "ops",
        Tab::Analytics => "analytics",
        Tab::MindMap => "map",
        Tab::Timeline => "timeline",
    }
}

fn grouping_name(grouping: Grouping) -> &'static str {
    match grouping {
        Grouping::Orchestrator => "parent",
        Grouping::Agent => "agent",
        Grouping::Model => "model",
        Grouping::Flat => "none",
    }
}

fn axis_name(axis: MapAxis) -> &'static str {
    match axis {
        MapAxis::Orchestrator => "parent",
        MapAxis::Branch => "branch",
        MapAxis::State => "state",
    }
}

fn parse_axis(name: &str) -> Option<MapAxis> {
    MapAxis::ALL
        .into_iter()
        .find(|axis| axis_name(*axis) == name)
}

fn scale_name(scale: TimeScale) -> &'static str {
    match scale {
        TimeScale::All => "all",
        TimeScale::Hour => "60m",
        TimeScale::Quarter => "15m",
    }
}

fn parse_scale(name: &str) -> Option<TimeScale> {
    TimeScale::ALL
        .into_iter()
        .find(|scale| scale_name(*scale) == name)
}

impl Prefs {
    pub(super) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "version": 2,
            "tab": tab_name(self.tab),
            "group": grouping_name(self.grouping),
            "scope": self.scope.name(),
            "mapAxis": axis_name(self.map_axis),
            "timeScale": scale_name(self.time_scale),
            // Cut as a read cuts it: a view written is read back whole.
            "filter": self.filter.chars().take(FILTER_MAX_CHARS).collect::<String>(),
        })
    }

    /// The view `text` keeps. Text that is not a JSON object is no view:
    /// `None`, and the caller starts from the defaults. Within an object,
    /// each value that is missing or not one this version knows is its
    /// default, the others are kept. Version 1 and 2 files read alike (the
    /// version is not checked): a version 1 file's `shown` panels are
    /// ignored — panel visibility maps onto no scope, and starting in a
    /// subset because a panel was once hidden would hide work — so it
    /// starts in 現在.
    pub(super) fn from_json(text: &str) -> Option<Prefs> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let object = value.as_object()?;
        let text_of = |key: &str| object.get(key).and_then(serde_json::Value::as_str);
        let base = Prefs::default();
        let filter: String = text_of("filter")
            .map(|query| one_line(query).chars().take(FILTER_MAX_CHARS).collect())
            .unwrap_or_default();
        Some(Prefs {
            tab: text_of("tab").and_then(Tab::parse).unwrap_or(base.tab),
            grouping: text_of("group")
                .and_then(Grouping::parse)
                .unwrap_or(base.grouping),
            scope: text_of("scope")
                .and_then(Scope::parse)
                .unwrap_or(base.scope),
            map_axis: text_of("mapAxis")
                .and_then(parse_axis)
                .unwrap_or(base.map_axis),
            time_scale: text_of("timeScale")
                .and_then(parse_scale)
                .unwrap_or(base.time_scale),
            filter,
        })
    }
}

/// The file the view is saved in, and what it holds as far as this run
/// knows: a view is written only when it differs from that.
#[derive(Debug)]
pub struct PrefsStore {
    path: PathBuf,
    saved: Option<Prefs>,
}

impl PrefsStore {
    pub fn at(path: PathBuf) -> Self {
        PrefsStore { path, saved: None }
    }

    /// The saved view, or the defaults when there is none or it cannot be
    /// read (missing, not a regular file, too large, not a view). What is
    /// read counts as saved.
    pub fn load(&mut self) -> Prefs {
        let prefs = read(&self.path).and_then(|text| Prefs::from_json(&text));
        self.saved.clone_from(&prefs);
        prefs.unwrap_or_default()
    }

    /// Takes `prefs` as what the file holds without writing it: the view a
    /// run starts with, the command line's values included, is not saved
    /// until the user changes it.
    pub(super) fn start_from(&mut self, prefs: Prefs) {
        self.saved = Some(prefs);
    }

    /// Writes `prefs` when it differs from what the file holds. A view that
    /// cannot be written is tried again at the next change.
    pub(super) fn save(&mut self, prefs: &Prefs) {
        if self.saved.as_ref() == Some(prefs) {
            return;
        }
        if write(&self.path, &prefs.to_json()).is_ok() {
            self.saved = Some(prefs.clone());
        }
    }
}

fn read(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > FILE_MAX_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// How many temporary names `write` tries before it gives up.
const TEMP_TRIES: usize = 8;

/// A temporary name nobody can guess: the process's random hash keys over
/// the time, the pid and a count.
fn random_name() -> String {
    use std::hash::{BuildHasher, Hasher};
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    hasher.write_u128(nanos);
    hasher.write_u32(std::process::id());
    hasher.write_u64(COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    format!("{:016x}", hasher.finish())
}

/// Writes through a temporary file renamed over `path`, so a reader (or a
/// crash) never sees half a view. The directory is made `0700`, the file
/// `0600`: the filter can name branches and tasks.
fn write(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    write_with(path, value, random_name)
}

/// `write`, its temporary file named `<path>.<name>.tmp` by `names`. The
/// file is made new (`create_new`, which follows no link): a name taken,
/// by anything, is passed over for the next. The file is removed when
/// anything after its creation fails.
fn write_with(
    path: &Path,
    value: &serde_json::Value,
    mut names: impl FnMut() -> String,
) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let (temp, mut file) = (0..TEMP_TRIES)
        .find_map(|_| {
            let mut temp = path.as_os_str().to_owned();
            temp.push(format!(".{}.tmp", names()));
            let temp = PathBuf::from(temp);
            let opened = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp);
            match opened {
                Ok(file) => Some(Ok((temp, file))),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(err) => Some(Err(err)),
            }
        })
        .unwrap_or_else(|| {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "no free temporary name",
            ))
        })?;
    let mut text = value.to_string();
    text.push('\n');
    let written = file.write_all(text.as_bytes());
    drop(file);
    written
        .and_then(|()| std::fs::rename(&temp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agent-dashboard-prefs-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("agent-dashboard").join("view.json")
    }

    fn sample() -> Prefs {
        Prefs {
            tab: Tab::Timeline,
            grouping: Grouping::Model,
            scope: Scope::Ended,
            map_axis: MapAxis::Branch,
            time_scale: TimeScale::Quarter,
            filter: "parent:release -state:done".into(),
        }
    }

    #[test]
    fn a_view_round_trips_through_its_file() {
        let path = temp("round-trip");
        let mut store = PrefsStore::at(path.clone());
        assert_eq!(store.load(), Prefs::default(), "no file: the defaults");
        store.save(&sample());
        let mut again = PrefsStore::at(path.clone());
        assert_eq!(again.load(), sample());
        let mode = |p: &Path| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(p).unwrap().permissions().mode() & 0o777
        };
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_corrupt_file_gives_the_defaults() {
        let path = temp("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for text in ["{\"tab\": \"map\"", "[1, 2]", "\u{0}\u{1}garbage", ""] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(
                PrefsStore::at(path.clone()).load(),
                Prefs::default(),
                "{text:?}"
            );
        }
        // Not a regular file: a directory, a symbolic link.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(PrefsStore::at(path.clone()).load(), Prefs::default());
        std::fs::remove_dir(&path).unwrap();
        let elsewhere = path.with_file_name("elsewhere.json");
        std::fs::write(&elsewhere, sample().to_json().to_string()).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert_eq!(PrefsStore::at(path.clone()).load(), Prefs::default());
        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_value_this_version_does_not_know_is_its_default_alone() {
        let text = r#"{"tab":"gantt-v9","group":"agent","scope":"finished",
            "mapAxis":"state","timeScale":7,"filter":"a\nb"}"#;
        let prefs = Prefs::from_json(text).unwrap();
        assert_eq!(prefs.tab, Tab::Operations);
        assert_eq!(prefs.grouping, Grouping::Agent);
        assert_eq!(prefs.scope, Scope::Current, "an unknown scope is 現在");
        assert_eq!(prefs.map_axis, MapAxis::State);
        assert_eq!(prefs.time_scale, TimeScale::All);
        assert_eq!(prefs.filter, "a b", "the filter is kept on one line");
        let long = format!(r#"{{"filter":"{}"}}"#, "x".repeat(5000));
        assert_eq!(
            Prefs::from_json(&long).unwrap().filter.len(),
            FILTER_MAX_CHARS
        );
    }

    /// What a version 1 dashboard wrote: its panels, no scope.
    const OLD_FILE: &str = r#"{"version":1,"tab":"ops","group":"model",
        "shown":{"queue":false,"running":true,"done":false,"timeline":true,"graph":false},
        "mapAxis":"branch","timeScale":"15m","filter":"parent:release"}"#;

    /// dashboard-worker-list AC10: an old file loads, in 現在, its other
    /// values kept.
    #[test]
    fn a_version_1_file_loads_in_the_current_scope() {
        let prefs = Prefs::from_json(OLD_FILE).expect("an old view is a view");
        assert_eq!(
            prefs,
            Prefs {
                tab: Tab::Operations,
                grouping: Grouping::Model,
                scope: Scope::Current,
                map_axis: MapAxis::Branch,
                time_scale: TimeScale::Quarter,
                filter: "parent:release".into(),
            }
        );
        for scope in Scope::ALL {
            let text = format!(r#"{{"scope":"{}"}}"#, scope.name());
            assert_eq!(Prefs::from_json(&text).unwrap().scope, scope);
        }
    }

    /// dashboard-worker-list AC10: version 2, the scope, no panels.
    #[test]
    fn a_view_is_written_as_version_2_with_its_scope() {
        let json = sample().to_json();
        assert_eq!(json["version"], 2);
        assert_eq!(json["scope"], "ended");
        assert!(json.get("shown").is_none(), "{json}");
    }

    /// dashboard-worker-list AC10: an old file is not rewritten at start,
    /// only at the first change of the view.
    #[test]
    fn an_old_file_is_rewritten_only_when_the_view_changes() {
        let path = temp("old-file");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, OLD_FILE).unwrap();
        let mut store = PrefsStore::at(path.clone());
        let loaded = store.load();
        store.start_from(loaded.clone());
        store.save(&loaded);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), OLD_FILE);
        let changed = Prefs {
            scope: Scope::Attention,
            ..loaded
        };
        store.save(&changed);
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains(r#""version":2"#), "{written}");
        assert_eq!(PrefsStore::at(path.clone()).load(), changed);
        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn the_same_view_is_not_written_again() {
        let path = temp("unchanged");
        let mut store = PrefsStore::at(path.clone());
        store.start_from(sample());
        store.save(&sample());
        assert!(!path.exists(), "the view a run starts with is not written");
        let mut changed = sample();
        changed.tab = Tab::Analytics;
        store.save(&changed);
        assert_eq!(PrefsStore::at(path.clone()).load(), changed);
        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn the_command_line_wins_over_the_saved_view() {
        let prefs = sample().overridden(Some(Tab::MindMap), None, Some(String::new()));
        assert_eq!(prefs.tab, Tab::MindMap);
        assert_eq!(prefs.grouping, Grouping::Model, "no --group: the saved one");
        assert_eq!(prefs.filter, "", "an empty --filter clears the saved one");
        assert_eq!(prefs.map_axis, MapAxis::Branch);
    }

    /// Review #4: a link waiting at the temporary file's old, predictable
    /// name (`<path>.<pid>.tmp`) is not written through.
    #[test]
    fn a_link_at_the_temporary_name_is_left_alone() {
        let path = temp("tmp-link");
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, "keep").unwrap();
        let mut planted = path.as_os_str().to_owned();
        planted.push(format!(".{}.tmp", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        PrefsStore::at(path.clone()).save(&sample());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(PrefsStore::at(path.clone()).load(), sample());
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    /// Review #4: a name taken is passed over for the next, and no
    /// temporary file is left.
    #[test]
    fn a_taken_temporary_name_is_passed_over() {
        let path = temp("tmp-taken");
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let taken = dir.join("view.json.taken.tmp");
        std::fs::write(&taken, "someone's").unwrap();
        let mut names = ["taken", "free"].into_iter().map(str::to_owned);
        write_with(&path, &sample().to_json(), || names.next().unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "someone's");
        assert_eq!(PrefsStore::at(path.clone()).load(), sample());
        assert!(
            !dir.join("view.json.free.tmp").exists(),
            "renamed into place"
        );
        // A failed write removes the file it made: here the rename onto a
        // directory fails.
        let blocked = dir.join("blocked");
        std::fs::create_dir_all(blocked.join("inside")).unwrap();
        let mut names = std::iter::once("mine".to_owned());
        assert!(write_with(&blocked, &sample().to_json(), || names.next().unwrap()).is_err());
        assert!(!dir.join("blocked.mine.tmp").exists());
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    /// Review #12: a view with a long filter is read back whole, its
    /// filter cut as a read cuts it.
    #[test]
    fn a_view_saved_with_a_long_filter_reads_back() {
        let path = temp("long-filter");
        let mut long = sample();
        long.filter = "x".repeat(70_000);
        PrefsStore::at(path.clone()).save(&long);
        let read = PrefsStore::at(path.clone()).load();
        assert_eq!(read.tab, Tab::Timeline);
        assert_eq!(read.filter, "x".repeat(FILTER_MAX_CHARS));
        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn the_default_path_sits_beside_noa_state() {
        if let Some(path) = default_path() {
            assert!(
                path.ends_with("noa/agent-dashboard/view.json"),
                "{}",
                path.display()
            );
        }
    }
}
