//! Pane list model: what each pane needs from you, and the order the menu
//! shows them in. Pure and GUI-agnostic so the ordering rules are testable.

use std::collections::{HashMap, HashSet};

use noa_ipc::Panel;

/// Short, flat status words (brand voice: no exclamation, no emoji).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneState {
    /// The pane raised attention (an agent asking, a notification). Until a
    /// dedicated approval-pending signal exists on the wire, `attention` is
    /// the best proxy for "waiting on you"; it already ranks first, so an
    /// approval state only has to map here to stay on top.
    Waiting,
    Running,
    Idle,
    /// Was running when last seen and has since gone quiet. Cleared when the
    /// pane runs again or you jump to it.
    Done,
}

impl PaneState {
    pub fn word(self) -> &'static str {
        match self {
            PaneState::Waiting => "waiting.",
            PaneState::Running => "running…",
            PaneState::Idle => "idle",
            PaneState::Done => "done.",
        }
    }

    /// Lower ranks sort first.
    fn rank(self) -> u8 {
        match self {
            PaneState::Waiting => 0,
            PaneState::Running => 1,
            PaneState::Idle => 2,
            PaneState::Done => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneRow {
    pub pane_id: u64,
    /// Line 1: `name · process`.
    pub title: String,
    /// Line 2: `branch · ~/cwd`.
    pub detail: String,
    pub state: PaneState,
}

/// Remembers the previous `busy` of each pane so a running → quiet
/// transition can read as `done.`.
#[derive(Default)]
pub struct Tracker {
    last_busy: HashMap<u64, bool>,
    done: HashSet<u64>,
}

impl Tracker {
    pub fn observe(&mut self, panels: &[Panel]) {
        let live: HashSet<u64> = panels.iter().map(|p| p.pane_id.0).collect();
        self.done.retain(|id| live.contains(id));
        self.last_busy.retain(|id, _| live.contains(id));
        for panel in panels {
            let id = panel.pane_id.0;
            let was_busy = self.last_busy.insert(id, panel.busy).unwrap_or(false);
            if panel.busy {
                self.done.remove(&id);
            } else if was_busy {
                self.done.insert(id);
            }
        }
    }

    /// You looked at it; it no longer needs to say `done.`.
    pub fn acknowledge(&mut self, pane_id: u64) {
        self.done.remove(&pane_id);
    }

    pub fn state(&self, panel: &Panel) -> PaneState {
        if panel.attention {
            PaneState::Waiting
        } else if panel.busy {
            PaneState::Running
        } else if self.done.contains(&panel.pane_id.0) {
            PaneState::Done
        } else {
            PaneState::Idle
        }
    }

    /// Rows in "needs you first" order. The sort is stable, so panes with the
    /// same state keep the server's (sidebar) order.
    pub fn rows(&self, panels: &[Panel], home: Option<&str>) -> Vec<PaneRow> {
        let mut rows: Vec<PaneRow> = panels
            .iter()
            .map(|panel| PaneRow {
                pane_id: panel.pane_id.0,
                title: title(panel),
                detail: detail(panel, home),
                state: self.state(panel),
            })
            .collect();
        rows.sort_by_key(|row| row.state.rank());
        rows
    }
}

const MAX_LINE_CHARS: usize = 44;

fn title(panel: &Panel) -> String {
    let name = panel.name.trim();
    let line = match panel.process.as_deref().map(str::trim).map(process_label) {
        Some(process) if !process.is_empty() && process != name => {
            if name.is_empty() {
                process.to_string()
            } else {
                format!("{name} · {process}")
            }
        }
        _ => name.to_string(),
    };
    truncate(&line)
}

/// Claude Code's native installer runs as its bare version (`2.1.203`); show
/// it as `claude`, as the sidebar does (`classify_agent` in noa-app).
fn process_label(process: &str) -> &str {
    let bare_version = process.contains('.')
        && process
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    if bare_version { "claude" } else { process }
}

fn detail(panel: &Panel, home: Option<&str>) -> String {
    let cwd = tilde(&panel.cwd, home);
    let line = match panel.branch.as_deref().filter(|b| !b.is_empty()) {
        Some(branch) if !cwd.is_empty() => format!("{branch} · {cwd}"),
        Some(branch) => branch.to_string(),
        None => cwd,
    };
    truncate(&line)
}

fn tilde(cwd: &str, home: Option<&str>) -> String {
    match home.filter(|h| !h.is_empty()) {
        Some(home) if cwd == home => "~".to_string(),
        Some(home) => match cwd.strip_prefix(home) {
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => cwd.to_string(),
        },
        None => cwd.to_string(),
    }
}

fn truncate(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let mut out: String = line.chars().take(MAX_LINE_CHARS - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_ipc::WireId;

    fn panel(id: u64, name: &str, busy: bool, attention: bool) -> Panel {
        Panel {
            window_group_id: WireId(1),
            window_id: WireId(1),
            pane_id: WireId(id),
            name: name.to_string(),
            cwd: "/Users/me/src/api".to_string(),
            branch: None,
            process: None,
            busy,
            attention,
            attachable: true,
            preview: Vec::new(),
        }
    }

    fn order(rows: &[PaneRow]) -> Vec<(u64, PaneState)> {
        rows.iter().map(|r| (r.pane_id, r.state)).collect()
    }

    #[test]
    fn attention_first_then_running_then_idle_then_done() {
        let mut tracker = Tracker::default();
        let first = [
            panel(1, "notes", true, false),
            panel(2, "infra", false, false),
            panel(3, "build", true, false),
            panel(4, "api", false, false),
        ];
        tracker.observe(&first);
        let now = [
            panel(1, "notes", false, false),
            panel(2, "infra", false, false),
            panel(3, "build", true, false),
            panel(4, "api", false, true),
        ];
        tracker.observe(&now);
        assert_eq!(
            order(&tracker.rows(&now, None)),
            vec![
                (4, PaneState::Waiting),
                (3, PaneState::Running),
                (2, PaneState::Idle),
                (1, PaneState::Done),
            ]
        );
    }

    #[test]
    fn equal_states_keep_server_order() {
        let tracker = Tracker::default();
        let panels = [
            panel(9, "a", true, false),
            panel(2, "b", true, false),
            panel(5, "c", true, false),
        ];
        let ids: Vec<u64> = tracker
            .rows(&panels, None)
            .iter()
            .map(|r| r.pane_id)
            .collect();
        assert_eq!(ids, vec![9, 2, 5]);
    }

    #[test]
    fn attention_outranks_busy_on_the_same_pane() {
        let tracker = Tracker::default();
        let p = panel(1, "api", true, true);
        assert_eq!(tracker.state(&p), PaneState::Waiting);
    }

    #[test]
    fn done_needs_an_observed_busy_and_clears_on_rerun_or_jump() {
        let mut tracker = Tracker::default();
        // First sighting of an idle pane is plain idle, not done.
        tracker.observe(&[panel(1, "a", false, false)]);
        assert_eq!(tracker.state(&panel(1, "a", false, false)), PaneState::Idle);

        tracker.observe(&[panel(1, "a", true, false)]);
        tracker.observe(&[panel(1, "a", false, false)]);
        assert_eq!(tracker.state(&panel(1, "a", false, false)), PaneState::Done);
        // Still done on the next quiet poll.
        tracker.observe(&[panel(1, "a", false, false)]);
        assert_eq!(tracker.state(&panel(1, "a", false, false)), PaneState::Done);

        tracker.acknowledge(1);
        assert_eq!(tracker.state(&panel(1, "a", false, false)), PaneState::Idle);

        tracker.observe(&[panel(1, "a", true, false)]);
        tracker.observe(&[panel(1, "a", false, false)]);
        tracker.observe(&[panel(1, "a", true, false)]);
        assert_eq!(
            tracker.state(&panel(1, "a", true, false)),
            PaneState::Running
        );
    }

    #[test]
    fn closed_panes_are_forgotten() {
        let mut tracker = Tracker::default();
        tracker.observe(&[panel(1, "a", true, false)]);
        tracker.observe(&[panel(1, "a", false, false)]);
        tracker.observe(&[]);
        assert!(tracker.done.is_empty() && tracker.last_busy.is_empty());
    }

    #[test]
    fn row_lines_show_name_process_branch_and_home_relative_cwd() {
        let tracker = Tracker::default();
        let mut p = panel(1, "api-server", false, true);
        p.process = Some("claude".to_string());
        p.branch = Some("feat/rate-limit".to_string());
        let row = &tracker.rows(&[p], Some("/Users/me"))[0];
        assert_eq!(row.title, "api-server · claude");
        assert_eq!(row.detail, "feat/rate-limit · ~/src/api");
        assert_eq!(row.state.word(), "waiting.");
    }

    #[test]
    fn process_equal_to_name_is_not_repeated_and_unknown_branch_is_omitted() {
        let tracker = Tracker::default();
        let mut p = panel(1, "zsh", false, false);
        p.process = Some("zsh".to_string());
        p.cwd = "/Users/me".to_string();
        let row = &tracker.rows(&[p], Some("/Users/me"))[0];
        assert_eq!(row.title, "zsh");
        assert_eq!(row.detail, "~");
    }

    #[test]
    fn claude_code_versioned_process_reads_as_claude() {
        let tracker = Tracker::default();
        let mut p = panel(1, "api-server", false, false);
        p.process = Some("2.1.304".to_string());
        assert_eq!(tracker.rows(&[p], None)[0].title, "api-server · claude");
        assert_eq!(process_label("node"), "node");
        assert_eq!(process_label("1."), "1.");
    }

    #[test]
    fn home_prefix_only_matches_whole_components() {
        assert_eq!(tilde("/Users/meow/x", Some("/Users/me")), "/Users/meow/x");
        assert_eq!(tilde("/Users/me/x", Some("/Users/me")), "~/x");
    }

    #[test]
    fn long_lines_are_truncated_with_an_ellipsis() {
        let long = "x".repeat(80);
        let out = truncate(&long);
        assert_eq!(out.chars().count(), MAX_LINE_CHARS);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn status_words_are_short_and_flat() {
        let words = [
            PaneState::Waiting,
            PaneState::Running,
            PaneState::Idle,
            PaneState::Done,
        ]
        .map(PaneState::word);
        assert_eq!(words, ["waiting.", "running…", "idle", "done."]);
        assert!(words.iter().all(|w| !w.contains('!')));
    }
}
