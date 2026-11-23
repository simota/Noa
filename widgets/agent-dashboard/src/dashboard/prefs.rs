//! The view kept across runs: the tab, the grouping, the panels shown, the
//! map's axis, the timeline's scale and the filter. Saved whenever one of
//! them changes and when the window closes, restored at start; the
//! command line's `--tab`, `--group` and `--filter` win over what was
//! saved. The file sits beside Noa's own session state, as the other
//! widgets keep theirs (`<data-dir>/noa/agent-dashboard/view.json`).

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::fleet::{Grouping, one_line};

use super::Tab;
use super::gantt::TimeScale;
use super::mindmap::MapAxis;
use super::panel::Panel;

/// The longest filter kept: a query is typed by hand.
const FILTER_MAX_CHARS: usize = 1000;
/// A file larger than this is not ours to read.
const FILE_MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefs {
    pub(super) tab: Tab,
    pub(super) grouping: Grouping,
    pub(super) shown: [bool; 5],
    pub(super) map_axis: MapAxis,
    pub(super) time_scale: TimeScale,
    pub(super) filter: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            tab: Tab::Operations,
            grouping: Grouping::Orchestrator,
            shown: [true; 5],
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

fn panel_name(panel: Panel) -> &'static str {
    match panel {
        Panel::Queue => "queue",
        Panel::Running => "running",
        Panel::Done => "done",
        Panel::Timeline => "timeline",
        Panel::Graph => "graph",
    }
}

impl Prefs {
    pub(super) fn to_json(&self) -> serde_json::Value {
        let shown: serde_json::Map<String, serde_json::Value> = Panel::ALL
            .iter()
            .map(|panel| {
                let on = self.shown[*panel as usize];
                (panel_name(*panel).to_owned(), on.into())
            })
            .collect();
        serde_json::json!({
            "version": 1,
            "tab": tab_name(self.tab),
            "group": grouping_name(self.grouping),
            "shown": shown,
            "mapAxis": axis_name(self.map_axis),
            "timeScale": scale_name(self.time_scale),
            "filter": self.filter,
        })
    }

    /// The view `text` keeps. Text that is not a JSON object is no view:
    /// `None`, and the caller starts from the defaults. Within an object,
    /// each value that is missing or not one this version knows is its
    /// default, the others are kept.
    pub(super) fn from_json(text: &str) -> Option<Prefs> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let object = value.as_object()?;
        let text_of = |key: &str| object.get(key).and_then(serde_json::Value::as_str);
        let base = Prefs::default();
        let mut shown = base.shown;
        if let Some(saved) = object.get("shown").and_then(serde_json::Value::as_object) {
            for panel in Panel::ALL {
                if let Some(on) = saved.get(panel_name(panel)).and_then(|v| v.as_bool()) {
                    shown[panel as usize] = on;
                }
            }
        }
        let filter: String = text_of("filter")
            .map(|query| one_line(query).chars().take(FILTER_MAX_CHARS).collect())
            .unwrap_or_default();
        Some(Prefs {
            tab: text_of("tab").and_then(Tab::parse).unwrap_or(base.tab),
            grouping: text_of("group")
                .and_then(Grouping::parse)
                .unwrap_or(base.grouping),
            shown,
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

/// Writes through a temporary file renamed over `path`, so a reader (or a
/// crash) never sees half a view. The directory is made `0700`, the file
/// `0600`: the filter can name branches and tasks.
fn write(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    let mut text = value.to_string();
    text.push('\n');
    file.write_all(text.as_bytes())?;
    drop(file);
    std::fs::rename(&temp, path).inspect_err(|_| {
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
            shown: [true, false, true, false, true],
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
        let text = r#"{"tab":"gantt-v9","group":"agent","shown":{"done":false,"graph":"no"},
            "mapAxis":"state","timeScale":7,"filter":"a\nb"}"#;
        let prefs = Prefs::from_json(text).unwrap();
        assert_eq!(prefs.tab, Tab::Operations);
        assert_eq!(prefs.grouping, Grouping::Agent);
        assert_eq!(prefs.shown, [true, true, false, true, true]);
        assert_eq!(prefs.map_axis, MapAxis::State);
        assert_eq!(prefs.time_scale, TimeScale::All);
        assert_eq!(prefs.filter, "a b", "the filter is kept on one line");
        let long = format!(r#"{{"filter":"{}"}}"#, "x".repeat(5000));
        assert_eq!(
            Prefs::from_json(&long).unwrap().filter.len(),
            FILTER_MAX_CHARS
        );
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
