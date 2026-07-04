use super::*;

/// Session persistence (`window-save-state`): capture the live window/tab/split
/// topology + per-pane cwd, and rebuild it on launch.
impl App {
    /// Capture the current topology into a serializable [`session::SessionState`].
    /// Windows are grouped by their AppKit tab group (`WindowGroupId`) so each
    /// logical window carries its tabs; the focused window/tab/pane are recorded
    /// as indices into that structure.
    fn capture_session(&self) -> session::SessionState {
        // Group native tabs by logical window, preserving `window_order`.
        let mut groups: Vec<(WindowGroupId, Vec<WindowId>)> = Vec::new();
        for window_id in &self.window_order {
            let Some(state) = self.windows.get(window_id) else {
                continue;
            };
            match groups.iter_mut().find(|(group, _)| *group == state.group) {
                Some((_, tabs)) => tabs.push(*window_id),
                None => groups.push((state.group, vec![*window_id])),
            }
        }

        let focused_group = self
            .focused
            .and_then(|id| self.windows.get(&id))
            .map(|s| s.group);
        let focused_window =
            focused_group.and_then(|group| groups.iter().position(|(g, _)| *g == group));

        let windows = groups
            .iter()
            .map(|(_, tabs)| self.capture_window(tabs))
            .collect();

        session::SessionState {
            windows,
            focused_window,
        }
    }

    fn capture_window(&self, tabs: &[WindowId]) -> session::WindowSession {
        let frame = tabs
            .first()
            .and_then(|id| self.windows.get(id))
            .map(|state| capture_window_frame(&state.window));
        let focused_tab = self
            .focused
            .and_then(|focused| tabs.iter().position(|id| *id == focused))
            .unwrap_or(0);
        let tab_sessions = tabs
            .iter()
            .filter_map(|id| {
                let state = self.windows.get(id)?;
                Some(self.capture_tab(*id, state))
            })
            .collect();
        session::WindowSession {
            frame,
            focused_tab,
            tabs: tab_sessions,
        }
    }

    fn capture_tab(&self, window_id: WindowId, state: &WindowState) -> session::TabSession {
        let split = self.split_tree_to_node(window_id, &state.split_tree);
        let mut leaves = Vec::new();
        collect_leaf_ids(&state.split_tree, &mut leaves);
        let focused_leaf = leaves
            .iter()
            .position(|pane| *pane == state.focused_pane)
            .unwrap_or(0);
        session::TabSession {
            focused_leaf,
            split,
        }
    }

    fn split_tree_to_node(&self, window_id: WindowId, tree: &SplitTree) -> session::PaneNode {
        match tree {
            SplitTree::Leaf { pane } => session::PaneNode::Leaf {
                cwd: self.pane_cwd(window_id, *pane),
            },
            SplitTree::Split {
                orientation,
                ratio,
                first,
                second,
            } => session::PaneNode::Split {
                orientation: orientation_to_session(*orientation),
                ratio: *ratio,
                first: Box::new(self.split_tree_to_node(window_id, first)),
                second: Box::new(self.split_tree_to_node(window_id, second)),
            },
        }
    }

    /// Persist the current session to disk (atomic). A no-op while restoring,
    /// when `window-save-state = never`, or when no windows are live — the last
    /// case deliberately leaves the previously written file intact so the
    /// close-last-window path still restores that final window next launch.
    pub(super) fn persist_session(&mut self) {
        if self.restoring || !self.config.window_save_state.restores() || self.windows.is_empty() {
            return;
        }
        let Some(path) = noa_config::session_state_path() else {
            return;
        };
        let state = self.capture_session();
        if let Err(err) = session::save(&path, &state) {
            log::warn!("failed to save session state: {err}");
        }
    }

    /// Restore the saved session on launch, if enabled and present. Suppressed
    /// entirely by `window-save-state = never` or an explicit CLI grid size. A
    /// missing/malformed/empty file is a silent no-op — startup is never
    /// blocked by session state.
    pub(super) fn restore_session_if_enabled(&mut self, event_loop: &ActiveEventLoop) {
        if !self.config.window_save_state.restores() || self.config.cli_grid_override {
            return;
        }
        let Some(path) = noa_config::session_state_path() else {
            return;
        };
        let Some(state) = session::load(&path) else {
            return;
        };
        if state.windows.is_empty() {
            return;
        }
        self.restoring = true;
        self.restore_session(event_loop, &state);
        self.restoring = false;
    }

    fn restore_session(&mut self, event_loop: &ActiveEventLoop, state: &session::SessionState) {
        // One entry per saved logical window: the native-tab `WindowId`s
        // spawned for it, in tab order, used to restore focus at the end.
        let mut restored_groups: Vec<Vec<WindowId>> = Vec::new();
        for window in &state.windows {
            let mut tab_ids = Vec::new();
            for tab in &window.tabs {
                // The first tab starts a fresh logical window (tab group);
                // the rest join it, matching how `new tab` vs `new window`
                // pick a group.
                let target = if tab_ids.is_empty() {
                    SpawnTarget::NewWindow
                } else {
                    SpawnTarget::CurrentWindow
                };
                let first_leaf_cwd = tab.split.first_leaf_cwd();
                let window_id =
                    match self.spawn_tab_with_cwd(event_loop, target, Some(first_leaf_cwd)) {
                        Ok(window_id) => window_id,
                        Err(err) => {
                            log::warn!("session restore: failed to spawn tab: {err}");
                            continue;
                        }
                    };
                tab_ids.push(window_id);
                self.materialize_tab(window_id, tab);
            }
            if let Some(first) = tab_ids.first() {
                self.apply_window_frame(*first, window.frame.as_ref());
            }
            restored_groups.push(tab_ids);
        }

        if let Some(focused_window) = state.focused_window
            && let (Some(group), Some(saved)) = (
                restored_groups.get(focused_window),
                state.windows.get(focused_window),
            )
            && let Some(window_id) = group.get(saved.focused_tab).or_else(|| group.first())
        {
            self.focused = Some(*window_id);
            if let Some(target) = self.windows.get(window_id) {
                target.window.clone().focus_window();
            }
        }
    }

    /// Rebuild a tab's saved split topology onto its just-spawned single pane.
    /// The initial pane becomes the tree's first (left-most) leaf — its cwd was
    /// already set at spawn — and fresh panes are spawned for every other leaf.
    fn materialize_tab(&mut self, window_id: WindowId, tab: &session::TabSession) {
        if tab.split.leaf_count() <= 1 {
            return;
        }
        let Some((root_pane, next_pane_id, placeholder_rect)) =
            self.windows.get(&window_id).map(|state| {
                (
                    state.focused_pane,
                    state.next_pane_id,
                    PaneRectApp::new(
                        0,
                        0,
                        state.surface_config.width,
                        state.surface_config.height,
                    ),
                )
            })
        else {
            return;
        };

        let mut minter = PaneMinter {
            next: next_pane_id,
            root: Some(root_pane),
        };
        let mut leaves = Vec::new();
        let tree = build_split_tree(&tab.split, &mut minter, &mut leaves);

        // Spawn surfaces for the non-root leaves before mutating window state
        // (`spawn_pane_surface` borrows `&self`). A rough grid/rect is fine —
        // `relayout_and_resize_window` fixes every pane's geometry below.
        let placeholder_grid = GridSize::new(self.config.cols, self.config.rows);
        let mut spawned = Vec::new();
        for leaf in &leaves {
            if leaf.is_root {
                continue;
            }
            match self.spawn_pane_surface(
                window_id,
                leaf.pane,
                placeholder_grid,
                placeholder_rect,
                leaf.cwd.clone(),
            ) {
                Ok(surface) => spawned.push((leaf.pane, surface)),
                Err(err) => log::warn!("session restore: failed to spawn split pane: {err}"),
            }
        }

        let focused_pane = leaves
            .get(tab.focused_leaf)
            .map(|leaf| leaf.pane)
            .unwrap_or(root_pane);
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.split_tree = tree;
            state.next_pane_id = minter.next;
            for (pane, surface) in spawned {
                state.surfaces.insert(pane, surface);
            }
            if state.surfaces.contains_key(&focused_pane) {
                state.focused_pane = focused_pane;
                state.last_mouse_pane = Some(focused_pane);
            }
        }
        self.relayout_and_resize_window(window_id);
    }

    fn apply_window_frame(&self, window_id: WindowId, frame: Option<&session::WindowFrame>) {
        let Some(frame) = frame else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        if let Some((x, y)) = frame.position {
            state.window.set_outer_position(LogicalPosition::new(x, y));
        }
        let _ = state
            .window
            .request_inner_size(LogicalSize::new(frame.width, frame.height));
    }
}

/// Mints pane ids while rebuilding a saved split tree, handing out the existing
/// initial pane (`root`) for the first leaf and fresh sequential ids after.
struct PaneMinter {
    next: u64,
    root: Option<PaneId>,
}

impl PaneMinter {
    /// Returns the next pane id and whether it is the reused initial pane.
    fn mint(&mut self) -> (PaneId, bool) {
        match self.root.take() {
            Some(pane) => (pane, true),
            None => {
                let pane = PaneId::new(self.next);
                self.next += 1;
                (pane, false)
            }
        }
    }
}

/// A leaf of a rebuilt split tree: its minted pane id, saved cwd, and whether
/// it reuses the tab's initial pane (whose surface already exists).
struct LeafSpec {
    pane: PaneId,
    cwd: Option<String>,
    is_root: bool,
}

/// Build a `SplitTree` from a saved [`session::PaneNode`], minting pane ids and
/// collecting the leaves in pre-order (matching [`collect_leaf_ids`] and the
/// serialized `focused_leaf` index).
fn build_split_tree(
    node: &session::PaneNode,
    minter: &mut PaneMinter,
    leaves: &mut Vec<LeafSpec>,
) -> SplitTree {
    match node {
        session::PaneNode::Leaf { cwd } => {
            let (pane, is_root) = minter.mint();
            leaves.push(LeafSpec {
                pane,
                cwd: cwd.clone(),
                is_root,
            });
            SplitTree::leaf(pane)
        }
        session::PaneNode::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            let first_tree = build_split_tree(first, minter, leaves);
            let second_tree = build_split_tree(second, minter, leaves);
            SplitTree::split(
                orientation_from_session(*orientation),
                *ratio,
                first_tree,
                second_tree,
            )
        }
    }
}

fn collect_leaf_ids(tree: &SplitTree, out: &mut Vec<PaneId>) {
    match tree {
        SplitTree::Leaf { pane } => out.push(*pane),
        SplitTree::Split { first, second, .. } => {
            collect_leaf_ids(first, out);
            collect_leaf_ids(second, out);
        }
    }
}

fn orientation_to_session(orientation: SplitOrientation) -> session::Orientation {
    match orientation {
        SplitOrientation::Horizontal => session::Orientation::Horizontal,
        SplitOrientation::Vertical => session::Orientation::Vertical,
    }
}

fn orientation_from_session(orientation: session::Orientation) -> SplitOrientation {
    match orientation {
        session::Orientation::Horizontal => SplitOrientation::Horizontal,
        session::Orientation::Vertical => SplitOrientation::Vertical,
    }
}

/// Read a window's logical-pixel frame (scale-independent) for persistence.
/// The position may be unavailable on some platforms; the size always is.
fn capture_window_frame(window: &Window) -> session::WindowFrame {
    let scale = window.scale_factor();
    let size = window.inner_size().to_logical::<f64>(scale);
    let position = window
        .outer_position()
        .ok()
        .map(|position| position.to_logical::<f64>(scale))
        .map(|position| (position.x, position.y));
    session::WindowFrame {
        position,
        width: size.width,
        height: size.height,
    }
}
