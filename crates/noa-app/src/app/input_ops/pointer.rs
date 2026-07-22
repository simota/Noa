use super::super::*;

/// How long a hover-path probe answer stays authoritative before a
/// background revalidation is kicked off (files appear and vanish under a
/// live shell).
const PATH_PROBE_TTL: std::time::Duration = std::time::Duration::from_secs(3);
/// Probe-cache size cap; completed entries are dropped wholesale when full
/// (hover churn is tiny — this only guards unbounded growth over a long
/// session), while in-flight entries always survive.
const PATH_PROBE_CACHE_CAP: usize = 1024;
/// Global cap on concurrently outstanding probe workers. A wedged network
/// volume holds its slot until the stat returns; once every slot is
/// occupied, new probes are deferred (retried by later hover events) rather
/// than spawning threads without bound.
const PATH_PROBE_MAX_IN_FLIGHT: usize = 16;

impl App {
    pub(in crate::app) fn apply_selection_gesture(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        gesture: SelectionGesture,
    ) {
        if gesture == SelectionGesture::None {
            return;
        }

        if let Some(surface) = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.surfaces.get_mut(&pane_id))
        {
            let mut terminal = surface.terminal.lock();
            match gesture {
                SelectionGesture::None => {}
                SelectionGesture::Clear { anchor } => {
                    terminal.clear_selection();
                    // Pin the drag anchor to content at press time; extending
                    // against this storage coordinate keeps the selection on
                    // the same text even if output scrolls mid-drag.
                    surface.selection_anchor = Some((
                        terminal.viewport_point_to_selection_point(anchor),
                        terminal.selection_rows_evicted(),
                    ));
                }
                SelectionGesture::Extend { anchor, focus } => {
                    let anchor = match surface.selection_anchor {
                        Some((point, evicted_then)) => {
                            // Rows evicted since capture shifted every storage
                            // coordinate up; re-align (a fully evicted anchor
                            // clamps to the oldest retained row).
                            let shift = terminal.selection_rows_evicted() - evicted_then;
                            if shift > point.y {
                                noa_grid::SelectionPoint::new(0, 0)
                            } else {
                                noa_grid::SelectionPoint::new(point.x, point.y - shift)
                            }
                        }
                        // No pinned anchor (e.g. tracking-mode handoff):
                        // fall back to the gesture's viewport anchor.
                        None => terminal.viewport_point_to_selection_point(anchor),
                    };
                    let focus = terminal.viewport_point_to_selection_point(focus);
                    terminal.set_selection(anchor, focus);
                }
                SelectionGesture::SelectWord(point) => {
                    surface.selection_anchor = None;
                    terminal.select_word_at_viewport_point(point)
                }
                SelectionGesture::SelectLine(point) => {
                    surface.selection_anchor = None;
                    terminal.select_line_at_viewport_point(point)
                }
            }
        }

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    pub(in crate::app) fn start_split_drag_at_last_mouse_point(
        &mut self,
        window_id: WindowId,
    ) -> bool {
        let Some(target) = self.split_drag_target_at_last_mouse_point(window_id) else {
            return false;
        };
        let Some(state) = self.windows.get_mut(&window_id) else {
            return false;
        };
        self.focused = Some(window_id);
        state.last_mouse_pane = None;
        state.active_split_drag = Some(target);
        true
    }

    pub(in crate::app) fn split_drag_target_at_last_mouse_point(
        &self,
        window_id: WindowId,
    ) -> Option<SplitResizeDrag> {
        let state = self.windows.get(&window_id)?;
        if state.zoomed.is_some() {
            return None;
        }
        let point = state.last_mouse_point?;
        // Same bounds as `relayout_and_resize_window`, so divider hit-testing
        // lines up with where the panes were actually laid out.
        let bounds = self.window_pane_bounds(window_id);
        split_resize_drag_target_at_point(&state.split_tree, bounds, point)
    }

    pub(in crate::app) fn drag_active_split(
        &mut self,
        window_id: WindowId,
        point: split_tree::Point,
    ) -> bool {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return false;
            };
            let Some(target) = state.active_split_drag.clone() else {
                return false;
            };
            resize_split_to_drag_point(&mut state.split_tree, &target, point);
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
        true
    }

    pub(in crate::app) fn finish_active_split_drag(&mut self, window_id: WindowId) -> bool {
        self.windows
            .get_mut(&window_id)
            .and_then(|state| state.active_split_drag.take())
            .is_some()
    }

    pub(in crate::app) fn pane_cell_at_position(
        &self,
        window_id: WindowId,
        position: PhysicalPosition<f64>,
        metrics: noa_font::Metrics,
    ) -> Option<(PaneId, Point)> {
        let state = self.windows.get(&window_id)?;
        let point = split_point_from_physical_position(position)?;
        let layout = visible_pane_ids(&state.split_tree, state.zoomed)
            .into_iter()
            .filter_map(|pane_id| {
                state
                    .surfaces
                    .get(&pane_id)
                    .map(|surface| (pane_id, surface.rect))
            })
            .collect::<Vec<_>>();
        let pane_id = match hit_test(&layout, point) {
            Some(HitTarget::Pane(pane_id)) => pane_id,
            Some(HitTarget::Divider) | None => return None,
        };
        let surface = state.surfaces.get(&pane_id)?;
        let local_x = position.x - f64::from(surface.rect.x);
        let local_y = position.y - f64::from(surface.rect.y);
        let cell = mouse::physical_position_to_grid_point(
            local_x,
            local_y,
            metrics.cell_w,
            metrics.cell_h,
            surface.grid_size,
            self.padding,
        );
        Some((pane_id, cell))
    }

    /// The Cmd+hover link under the mouse in `window_id`'s focused-under-
    /// pointer pane, if `Cmd` is held and the cell under `last_mouse_cell`
    /// carries an OSC 8 hyperlink or sits inside an auto-detected
    /// `https?://` URL run. Reuses `last_mouse_pane`/`last_mouse_cell`
    /// (already kept up to date by every `CursorMoved`) instead of
    /// recomputing a pixel hit-test, so it can also be called from
    /// `ModifiersChanged` with the mouse stationary.
    pub(in crate::app) fn hover_link_target(
        &mut self,
        window_id: WindowId,
    ) -> Option<(PaneId, HoverLink)> {
        if !self.modifiers.super_key() {
            return None;
        }
        let state = self.windows.get(&window_id)?;
        let pane_id = state.last_mouse_pane?;
        let surface = state.surfaces.get(&pane_id)?;
        let cell = surface.last_mouse_cell?;

        let terminal = surface.terminal.lock();
        let row = terminal.active().visible_row(cell.y)?;
        if let Some(link_id) = row.cells.get(cell.x as usize).and_then(|c| c.hyperlink) {
            return Some((pane_id, HoverLink::Registry(link_id.get())));
        }
        if let Some(url) = noa_grid::detect_url_at_column(&row, cell.x) {
            return Some((
                pane_id,
                HoverLink::Range {
                    y: cell.y,
                    x_start: url.start_x,
                    x_end: url.end_x,
                },
            ));
        }
        // Paths in a remote pane's output live on the remote host — never
        // detect (let alone open) them as local files, even when a same-named
        // local file happens to exist.
        if surface.is_remote() {
            return None;
        }
        // Path detection needs no more of the terminal than the row already
        // borrowed and `cwd` for relative-path resolution; drop the lock
        // before the existence probe below.
        let path_match = noa_grid::detect_path_at_column(&row, cell.x);
        let cwd = terminal.cwd.clone();
        drop(terminal);
        let path_match = path_match?;
        let resolved = resolve_hover_path(&path_match.path, cwd.as_deref())?;
        if !self.probe_path_exists(window_id, resolved) {
            return None;
        }
        Some((
            pane_id,
            HoverLink::Range {
                y: cell.y,
                x_start: path_match.start_x,
                x_end: path_match.end_x,
            },
        ))
    }

    /// Whether `path` is known to exist, per the probe cache. A miss (or an
    /// entry older than [`PATH_PROBE_TTL`]) answers `false` *now* and kicks
    /// off a worker-thread `stat` — the filesystem is never touched on the
    /// main thread, where a slow or wedged network volume would stall
    /// rendering and input. The worker posts [`UserEvent::PathProbe`], whose
    /// handler re-syncs the hover link so a confirmed path underlines
    /// without the mouse having to move again.
    fn probe_path_exists(&mut self, window_id: WindowId, path: std::path::PathBuf) -> bool {
        let now = Instant::now();
        // Global bound on worker threads: every wedged stat occupies its
        // slot until it answers, so hovering many distinct paths on a dead
        // volume defers new probes instead of spawning without limit (a
        // deferred path is simply retried by a later hover event).
        let slot_free = self
            .path_probe_cache
            .values()
            .filter(|entry| entry.in_flight.is_some())
            .count()
            < PATH_PROBE_MAX_IN_FLIGHT;
        if let Some(entry) = self.path_probe_cache.get_mut(&path) {
            if entry.in_flight.is_some() {
                // At most one worker probe per path: a stat wedged on a dead
                // network volume must not spawn a sibling on every pointer
                // move. Remember who's asking so the answer re-syncs them.
                if !entry.waiters.contains(&window_id) {
                    entry.waiters.push(window_id);
                }
                return entry.answer == Some(true);
            }
            let answer = entry.answer;
            if now.duration_since(entry.at) < PATH_PROBE_TTL || !slot_free {
                // Fresh — or expired with every probe slot occupied, in
                // which case the stale answer keeps being served until a
                // slot frees up (`at` deliberately isn't bumped, so any
                // later hover retries the revalidation).
                return answer == Some(true);
            }
            // Expired: serve the stale answer (no underline flicker on a
            // stationary hover) and revalidate in the background.
            entry.in_flight = Some(self.next_path_probe_generation);
            entry.waiters = vec![window_id];
            self.start_path_probe(path);
            return answer == Some(true);
        }
        if !slot_free {
            return false;
        }
        if self.path_probe_cache.len() >= PATH_PROBE_CACHE_CAP {
            // Evict only completed entries: dropping an in-flight one would
            // orphan its worker (its answer then dies as a generation
            // mismatch) and let the next hover of the same path spawn a
            // sibling thread — unbounded when a volume has stats wedged.
            // In-flight survivors keep the map at most cap + wedged-probes.
            self.path_probe_cache
                .retain(|_, entry| entry.in_flight.is_some());
        }
        self.path_probe_cache.insert(
            path.clone(),
            PathProbeEntry {
                answer: None,
                at: now,
                in_flight: Some(self.next_path_probe_generation),
                waiters: vec![window_id],
            },
        );
        self.start_path_probe(path);
        false
    }

    /// Whether `path`'s last probe answered "exists" — the click-time gate:
    /// only a path the hover machinery actually confirmed may be opened.
    pub(in crate::app) fn path_probe_confirmed(&self, path: &std::path::Path) -> bool {
        self.path_probe_cache
            .get(path)
            .is_some_and(|entry| entry.answer == Some(true))
    }

    /// Stat `path` on a detached worker and post the answer back to the
    /// event loop as [`UserEvent::PathProbe`], stamped with the generation
    /// the caller just recorded in the entry's `in_flight` (consumed here).
    fn start_path_probe(&mut self, path: std::path::PathBuf) {
        let generation = self.next_path_probe_generation;
        self.next_path_probe_generation += 1;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let exists = path.exists();
            let _ = proxy.send_event(UserEvent::PathProbe {
                generation,
                path,
                exists,
            });
        });
    }

    /// Recompute the Cmd+hover target for `window_id` and reconcile it into
    /// `Surface::hover_link` + the window's cursor icon. Called from every
    /// event that can change the answer: `CursorMoved` (pointer or pane
    /// moved) and `ModifiersChanged` (Cmd pressed/released with the mouse
    /// stationary).
    pub(in crate::app) fn sync_hover_link(&mut self, window_id: WindowId) {
        let target = self.hover_link_target(window_id);
        let target_pane = target.as_ref().map(|(pane_id, _)| *pane_id);

        // Clear a stale hover on whichever pane held it previously, if the
        // target has moved to a different pane/window or disappeared. This
        // is the only place a hover can go stale outside its own pane: a
        // pane's own hover_link is otherwise only ever written here.
        if let Some((prev_window, prev_pane)) = self.hovered_link
            && (prev_window != window_id || Some(prev_pane) != target_pane)
        {
            let cleared = self
                .windows
                .get_mut(&prev_window)
                .and_then(|state| state.surfaces.get_mut(&prev_pane))
                .is_some_and(|surface| surface.hover_link.take().is_some());
            if cleared && let Some(state) = self.windows.get(&prev_window) {
                state.window.request_redraw();
            }
            self.hovered_link = None;
        }

        if let Some((pane_id, link)) = target {
            self.hovered_link = Some((window_id, pane_id));
            let changed = self
                .windows
                .get_mut(&window_id)
                .and_then(|state| state.surfaces.get_mut(&pane_id))
                .is_some_and(|surface| {
                    let changed = surface.hover_link != Some(link);
                    surface.hover_link = Some(link);
                    changed
                });
            if changed && let Some(state) = self.windows.get(&window_id) {
                state.window.request_redraw();
            }
        }

        self.update_cursor_icon(window_id);
    }

    /// Pointer cursor while a link is Cmd+hovered in `window_id`'s
    /// under-the-mouse pane, the platform default otherwise.
    pub(in crate::app) fn update_cursor_icon(&self, window_id: WindowId) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let hovering = state.sidebar_button_hover
            || state
                .last_mouse_pane
                .and_then(|pane_id| state.surfaces.get(&pane_id))
                .is_some_and(|surface| surface.hover_link.is_some());
        state.window.set_cursor(if hovering {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        });
    }

    /// Resolve the currently Cmd+hovered link in `window_id`'s under-the-
    /// mouse pane to its open target, re-deriving it from live grid state
    /// (rather than caching it on `Surface::hover_link`, which the renderer
    /// only needs the geometry of). A path target's line/column suffix is
    /// dropped here — `open` has no notion of a line number — but it was
    /// still part of what got underlined on hover.
    pub(in crate::app) fn open_hovered_link(&self, window_id: WindowId) -> Option<LinkTarget> {
        let state = self.windows.get(&window_id)?;
        let pane_id = state.last_mouse_pane?;
        let surface = state.surfaces.get(&pane_id)?;
        let hover_link = surface.hover_link?;
        let cell = surface.last_mouse_cell?;

        let terminal = surface.terminal.lock();
        let row = terminal.active().visible_row(cell.y)?;
        if let Some(link_id) = row.cells.get(cell.x as usize).and_then(|c| c.hyperlink) {
            return terminal
                .hyperlinks
                .get(link_id.get())
                .map(|link| LinkTarget::Uri(link.uri.clone()));
        }
        if let Some(url) = noa_grid::detect_url_at_column(&row, cell.x) {
            return Some(LinkTarget::Uri(url.uri));
        }
        // Same remote gate as `hover_link_target` — a remote pane's paths
        // are another host's files.
        if surface.is_remote() {
            return None;
        }
        let path_match = noa_grid::detect_path_at_column(&row, cell.x);
        let cwd = terminal.cwd.clone();
        drop(terminal);
        let path_match = path_match?;
        // The row may have been rewritten under a stationary pointer (a PTY
        // redraw doesn't re-sync the hover), leaving `hover_link` pointing
        // at text that no longer matches what was underlined. Only open a
        // path the hover machinery actually vetted: same geometry as the
        // live hover, and a probe that answered "exists".
        let expected = HoverLink::Range {
            y: cell.y,
            x_start: path_match.start_x,
            x_end: path_match.end_x,
        };
        if hover_link != expected {
            return None;
        }
        let resolved = resolve_hover_path(&path_match.path, cwd.as_deref())?;
        self.path_probe_confirmed(&resolved)
            .then_some(LinkTarget::Path(resolved))
    }
}

/// Resolve a detected path token to an absolute filesystem path: `~/...`
/// expands against `$HOME`, `/...` is used as-is, and anything else is
/// joined onto `cwd` (the owning pane's `Terminal::cwd`, from OSC 7).
/// Returns `None` when a relative path has no `cwd` to resolve against —
/// noa-grid's bare-relative-path heuristic (any token with an interior `/`)
/// only stays safe once paired with both this resolution *and* the
/// existence check the caller performs on the result.
fn resolve_hover_path(path: &str, cwd: Option<&str>) -> Option<std::path::PathBuf> {
    resolve_hover_path_with(path, cwd, std::env::var_os("HOME").as_deref())
}

/// Pure core of [`resolve_hover_path`] with `$HOME` injected, so tests can
/// exercise `~/` expansion without mutating process-global environment
/// state (cargo runs tests multi-threaded; `set_var` is unsafe there).
fn resolve_hover_path_with(
    path: &str,
    cwd: Option<&str>,
    home: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(std::path::Path::new(home?).join(rest));
    }
    if path.starts_with('/') {
        return Some(std::path::PathBuf::from(path));
    }
    Some(std::path::Path::new(cwd?).join(path))
}

#[cfg(test)]
mod path_resolution_tests {
    use super::resolve_hover_path_with;
    use std::ffi::OsStr;

    #[test]
    fn resolves_absolute_path_as_is() {
        assert_eq!(
            resolve_hover_path_with("/usr/bin/env", Some("/home/x"), None),
            Some(std::path::PathBuf::from("/usr/bin/env"))
        );
    }

    #[test]
    fn resolves_home_relative_path_against_home() {
        assert_eq!(
            resolve_hover_path_with("~/notes/todo.md", None, Some(OsStr::new("/Users/example"))),
            Some(std::path::PathBuf::from("/Users/example/notes/todo.md"))
        );
    }

    #[test]
    fn refuses_home_relative_path_with_no_home() {
        assert_eq!(resolve_hover_path_with("~/notes/todo.md", None, None), None);
    }

    #[test]
    fn resolves_bare_relative_path_against_cwd() {
        assert_eq!(
            resolve_hover_path_with("src/main.rs", Some("/repo"), None),
            Some(std::path::PathBuf::from("/repo/src/main.rs"))
        );
    }

    #[test]
    fn refuses_relative_path_with_no_cwd() {
        assert_eq!(resolve_hover_path_with("src/main.rs", None, None), None);
    }
}
