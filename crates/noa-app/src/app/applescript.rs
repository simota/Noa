//! AppleScript / Apple Event bridge glue (applescript spec). Installs the
//! Apple Event handlers once and keeps the read-only [`AppStateSnapshot`] the
//! handler answers property queries from up to date, both on the main thread.

use std::hash::{Hash, Hasher};

use super::*;
use crate::macos_applescript::{
    AppStateSnapshot, Registration, TabSnapshot, TerminalSnapshot, WindowSnapshot,
};

/// Coarse refresh interval for the AppleScript snapshot's lock-bearing fields
/// (terminal cwd/title). Structural changes rebuild immediately; this only
/// bounds how stale a cwd read can be under sustained output without a
/// structural change.
const APPLESCRIPT_SNAPSHOT_REFRESH: Duration = Duration::from_millis(500);

impl App {
    /// Register the Apple Event handlers once, after the app is running
    /// (applescript R-2/Amendment 3). A no-op when `macos-applescript` is false
    /// or off macOS; a failed registration is left as `None`, not fatal.
    pub(super) fn install_applescript_if_needed(&mut self) {
        if self.applescript_install_attempted {
            return;
        }
        self.applescript_install_attempted = true;
        if !self.config.macos_applescript {
            return;
        }
        self.applescript =
            Registration::install(self.proxy.clone(), self.applescript_snapshot.clone());
    }

    /// Rebuild the window/tab/terminal projection the Apple Event handler reads
    /// (applescript Amendment 1.1). Called from `about_to_wait` while the bridge
    /// is installed, so a property query always sees the latest topology/cwd/
    /// focus without the handler ever waiting on the event loop. A no-op when
    /// the bridge was never installed.
    pub(super) fn sync_applescript_snapshot(&mut self) {
        if self.applescript.is_none() {
            return;
        }

        // Cheap change-detection first: a signature over topology/focus/titles
        // that needs no terminal lock. Rebuild the (lock-bearing) snapshot only
        // when it changed or the coarse refresh interval elapsed, so sustained
        // pty output doesn't lock every pane on every event-loop wake.
        let sig = self.applescript_structural_signature();
        let due = self
            .applescript_snapshot_at
            .is_none_or(|at| at.elapsed() >= APPLESCRIPT_SNAPSHOT_REFRESH);
        if sig == self.applescript_snapshot_sig && !due {
            return;
        }
        self.applescript_snapshot_sig = sig;
        self.applescript_snapshot_at = Some(Instant::now());

        let mut windows: Vec<WindowSnapshot> = Vec::new();
        let mut group_pos: HashMap<WindowGroupId, usize> = HashMap::new();
        for window_id in &self.window_order {
            let Some(state) = self.windows.get(window_id) else {
                continue;
            };
            let win_pos = *group_pos.entry(state.group).or_insert_with(|| {
                let index = windows.len() + 1;
                windows.push(WindowSnapshot {
                    id: state.group.0,
                    name: String::new(),
                    index,
                    tabs: Vec::new(),
                });
                windows.len() - 1
            });

            // Terminals in a stable order (by PaneId), so `terminal N` is
            // deterministic across snapshots.
            let mut panes: Vec<PaneId> = state.surfaces.keys().copied().collect();
            panes.sort_by_key(|pane| pane.get());
            let terminals = panes
                .iter()
                .enumerate()
                .filter_map(|(k, pane)| {
                    let surface = state.surfaces.get(pane)?;
                    let (title, cwd) = {
                        let terminal = surface.terminal.lock();
                        (terminal.title.clone(), terminal.cwd.clone())
                    };
                    let name = if !title.is_empty() {
                        title
                    } else {
                        cwd.clone().unwrap_or_default()
                    };
                    Some(TerminalSnapshot {
                        id: pane.get(),
                        name,
                        index: k + 1,
                        selected: state.focused_pane == *pane,
                        cwd,
                    })
                })
                .collect();

            let tab_name = state
                .title_override
                .clone()
                .unwrap_or_else(|| state.title.clone());
            let tabs = &mut windows[win_pos].tabs;
            let tab_index = tabs.len() + 1;
            tabs.push(TabSnapshot {
                id: u64::from(*window_id),
                name: tab_name,
                index: tab_index,
                selected: self.focused == Some(*window_id),
                terminals,
            });
        }

        // A window's name mirrors its first tab's name (Ghostty shows the
        // active tab's title in the window's AppleScript name).
        for window in &mut windows {
            if let Some(first) = window.tabs.first() {
                window.name = first.name.clone();
            }
        }

        *self.applescript_snapshot.lock() = AppStateSnapshot {
            frontmost: self.os_focused.is_some(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            windows,
        };
    }

    /// A lock-free signature over everything the AppleScript snapshot derives
    /// from *except* terminal cwd/title (which need a lock): window order and
    /// groups, focus, per-tab focused pane + pane-id set, and tab titles. A
    /// change here forces an immediate rebuild; cwd/title drift is caught by the
    /// coarse time-based refresh instead.
    fn applescript_structural_signature(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.focused.map(u64::from).hash(&mut hasher);
        self.os_focused.map(u64::from).hash(&mut hasher);
        for window_id in &self.window_order {
            let Some(state) = self.windows.get(window_id) else {
                continue;
            };
            u64::from(*window_id).hash(&mut hasher);
            state.group.0.hash(&mut hasher);
            state.focused_pane.get().hash(&mut hasher);
            state.title_override.hash(&mut hasher);
            state.title.hash(&mut hasher);
            let mut panes: Vec<u64> = state.surfaces.keys().map(|pane| pane.get()).collect();
            panes.sort_unstable();
            panes.hash(&mut hasher);
        }
        hasher.finish()
    }
}
