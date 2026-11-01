//! AppleScript / Apple Event bridge glue (applescript + agent-automation
//! specs). Installs the Apple Event handlers once, keeps the read-only
//! [`AppStateSnapshot`] the handler answers property queries from up to date,
//! and executes the [`ScriptAction`]s the handler queues — all on the main
//! thread.

use std::hash::{Hash, Hasher};

use super::*;
use crate::macos_applescript::{
    AppStateSnapshot, Registration, ScriptAction, ScriptMouse, ScriptSplit, SurfaceConfig,
    TabSnapshot, TerminalSnapshot, WindowSnapshot, merge_pending, terminal_name,
};

/// Coarse refresh interval for the AppleScript snapshot's lock-bearing fields
/// (terminal cwd/title). Structural changes rebuild immediately; this only
/// bounds how stale a cwd read can be under sustained output without a
/// structural change.
const APPLESCRIPT_SNAPSHOT_REFRESH: Duration = Duration::from_millis(500);

/// What `wait after command` prints when the process exits (agent-automation
/// REQ-005).
const PROCESS_EXITED_MESSAGE: &[u8] =
    b"\r\n\x1b[0m[Process exited. Press any key to close the terminal.]";

/// Overrides for the spawn a scripted action is running (agent-automation
/// L2.1): the ids the AE handler already handed out, and the parts of a
/// `surface configuration` the pty spawn consumes. Set by the executor around
/// one spawn and read by `spawn_pane_surface`/`allocate_group_id`/`new_split`.
#[derive(Default)]
pub(super) struct ScriptSpawn {
    pub(super) pane_id: Option<PaneId>,
    pub(super) tab_id: Option<u64>,
    pub(super) group_id: Option<u64>,
    pub(super) cwd: Option<String>,
    pub(super) argv: Option<Vec<String>>,
    pub(super) env: Vec<(String, String)>,
    pub(super) wait_after_command: bool,
}

impl ScriptSpawn {
    fn from_config(config: &SurfaceConfig) -> Self {
        Self {
            cwd: usable_cwd(config.cwd.clone()),
            // `command` replaces the shell (agent-automation Amendment 1),
            // through `/bin/sh -c` like a typed command line.
            argv: config
                .command
                .clone()
                .map(|command| vec!["/bin/sh".to_string(), "-c".to_string(), command]),
            env: config.env.clone(),
            wait_after_command: config.wait_after_command,
            ..Self::default()
        }
    }
}

/// A configured `initial working directory`, or `None` when it is not an
/// existing directory — the spawn then falls back the way it does without
/// one (focused shell's cwd, else inherited) instead of failing (L2.4).
fn usable_cwd(cwd: Option<String>) -> Option<String> {
    cwd.filter(|dir| std::path::Path::new(dir).is_dir())
}

/// A scripted `font size` in points, clamped to the runtime range while
/// still `f64`, so a huge finite value cannot overflow to infinity in the
/// `f32` conversion (L2.4).
fn script_font_points(points: f64) -> f32 {
    points.clamp(
        f64::from(MIN_RUNTIME_FONT_SIZE),
        f64::from(MAX_RUNTIME_FONT_SIZE),
    ) as f32
}

/// Bounds on one scripted scroll: far beyond any real wheel or trackpad
/// event, yet small enough that every downstream conversion (rows to scroll,
/// alternate-scroll arrow bytes) stays allocatable.
const MAX_SCROLL_NOTCHES: f64 = 1_000.0;
const MAX_SCROLL_POINTS: f64 = 100_000.0;

/// The winit wheel delta for a scripted `send mouse scroll` (L2.2): whole
/// notches truncated toward 0, or points scaled to physical pixels, each
/// clamped so a huge finite value (`1.0E+20`) cannot overflow a row count.
pub(super) fn script_scroll_delta(
    dx: f64,
    dy: f64,
    precise: bool,
    scale_factor: f64,
) -> MouseScrollDelta {
    if precise {
        let clamp = |v: f64| v.clamp(-MAX_SCROLL_POINTS, MAX_SCROLL_POINTS) * scale_factor;
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(clamp(dx), clamp(dy)))
    } else {
        let clamp = |v: f64| v.trunc().clamp(-MAX_SCROLL_NOTCHES, MAX_SCROLL_NOTCHES) as f32;
        MouseScrollDelta::LineDelta(clamp(dx), clamp(dy))
    }
}

/// Whether a key leaves a pane held by `wait after command` open: a bare
/// modifier, or a Cmd shortcut (agent-automation REQ-005).
pub(super) fn closes_held_pane_exempt(key: &Key, modifiers: ModifiersState) -> bool {
    modifiers.super_key() || matches!(key, Key::Named(named) if is_modifier_key(*named))
}

/// Whether an IME event is the user typing — a composition starting or
/// text committing — and so counts as the key press that closes a pane held
/// by `wait after command`.
pub(super) fn ime_event_types_text(event: &Ime) -> bool {
    match event {
        Ime::Preedit(text, _) | Ime::Commit(text) => !text.is_empty(),
        Ime::Enabled | Ime::Disabled => false,
    }
}

fn is_modifier_key(key: NamedKey) -> bool {
    matches!(
        key,
        NamedKey::Shift
            | NamedKey::Control
            | NamedKey::Alt
            | NamedKey::Super
            | NamedKey::Meta
            | NamedKey::Hyper
            | NamedKey::CapsLock
            | NamedKey::Fn
            | NamedKey::FnLock
            | NamedKey::NumLock
    )
}

/// Map a script mouse position (points from the pane's top-left) to the
/// window-physical position a real pointer there would report, clamped inside
/// the pane (agent-automation L2.4). Delivery to the pane is guaranteed by
/// `App::script_mouse_pane`, not by moving the point, so the cell it maps to
/// is exactly the one a pointer there would hit.
pub(super) fn script_point_to_physical(
    rect: PaneRectApp,
    scale_factor: f64,
    x: f64,
    y: f64,
) -> PhysicalPosition<f64> {
    let clamp = |v: f64, len: u32| v.clamp(0.0, f64::from(len.saturating_sub(1)));
    PhysicalPosition::new(
        f64::from(rect.x) + clamp(x * scale_factor, rect.w),
        f64::from(rect.y) + clamp(y * scale_factor, rect.h),
    )
}

/// Whether a key event is the rest of a key whose press closed a held pane
/// (its auto-repeat or release), updating `keys`: the release retires the
/// key, and a fresh press means the release was lost (focus moved away), so
/// it retires the key and goes through.
pub(super) fn swallow_held_pane_close_key(
    keys: &mut HashSet<PhysicalKey>,
    key: PhysicalKey,
    pressed: bool,
    repeat: bool,
) -> bool {
    if !keys.contains(&key) {
        return false;
    }
    if pressed && repeat {
        return true;
    }
    keys.remove(&key);
    !pressed
}

/// Where a scripted button or wheel event lands, in window-physical pixels:
/// the real pointer when it is over the pane, else the pane's saved scripted
/// position (pane-relative points) re-mapped onto its current `rect` and
/// scale, else the pane's top-left.
fn script_press_position(
    real: Option<PhysicalPosition<f64>>,
    saved: Option<(f64, f64)>,
    rect: PaneRectApp,
    scale_factor: f64,
) -> PhysicalPosition<f64> {
    real.unwrap_or_else(|| {
        let (x, y) = saved.unwrap_or((0.0, 0.0));
        script_point_to_physical(rect, scale_factor, x, y)
    })
}

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
            Registration::install(self.proxy.clone(), self.applescript_shared.clone());
    }

    /// The next pane id for a spawn: the AE handler's pre-allocated one while
    /// a scripted spawn runs, a fresh one otherwise.
    pub(super) fn next_pane_id(&mut self) -> PaneId {
        self.script_spawn
            .as_mut()
            .and_then(|spawn| spawn.pane_id.take())
            .unwrap_or_else(PaneId::alloc)
    }

    /// Rebuild the window/tab/terminal projection the Apple Event handler
    /// reads (applescript Amendment 1.1). Called from `about_to_wait` while the
    /// bridge is installed, so a property query always sees the latest
    /// topology/cwd/focus without the handler ever waiting on the event loop.
    /// Pending objects the handler promised are re-merged on every rebuild.
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
        let mut front_window = None;
        for window_id in &self.window_order {
            let Some(state) = self.windows.get(window_id) else {
                continue;
            };
            let win_pos = *group_pos.entry(state.group).or_insert_with(|| {
                windows.push(WindowSnapshot {
                    id: state.group.0,
                    name: String::new(),
                    tabs: Vec::new(),
                    pending: false,
                });
                windows.len() - 1
            });
            if self.focused == Some(*window_id) {
                front_window = Some(win_pos);
            }

            // Terminals in a stable order (by PaneId), so `terminal N` is
            // deterministic across snapshots.
            let mut panes: Vec<PaneId> = state.surfaces.keys().copied().collect();
            panes.sort_by_key(|pane| pane.get());
            let terminals = panes
                .iter()
                .filter_map(|pane| {
                    let surface = state.surfaces.get(pane)?;
                    let (title, cwd) = {
                        let terminal = surface.terminal.lock();
                        (terminal.title.clone(), terminal.cwd.clone())
                    };
                    let name = terminal_name(&title, cwd.as_deref());
                    Some(TerminalSnapshot {
                        id: pane.get(),
                        name,
                        selected: state.focused_pane == *pane,
                        cwd,
                        pid: surface.child_pid,
                        tty: surface.tty_name.clone(),
                        terminal: Some(surface.terminal.clone()),
                        pending: false,
                    })
                })
                .collect();

            let tab_name = state
                .title_override
                .clone()
                .unwrap_or_else(|| state.title.clone());
            // A tab is `selected` when it is its window's front native tab.
            let selected = crate::macos_window::native_tab_is_selected(&state.window)
                .unwrap_or(self.focused == Some(*window_id));
            windows[win_pos].tabs.push(TabSnapshot {
                id: state.tab_id,
                name: tab_name,
                selected,
                terminals,
                pending: false,
            });
        }

        // A window's name mirrors its selected tab's name (Ghostty shows the
        // active tab's title in the window's AppleScript name).
        for window in &mut windows {
            if let Some(tab) = window
                .tabs
                .iter()
                .find(|tab| tab.selected)
                .or_else(|| window.tabs.first())
            {
                window.name = tab.name.clone();
            }
        }

        let mut snapshot = AppStateSnapshot {
            frontmost: self.os_focused.is_some(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            front_window: front_window.or((!windows.is_empty()).then_some(0)),
            windows,
        };
        merge_pending(&mut snapshot, &mut self.applescript_shared.pending.lock());
        *self.applescript_shared.snapshot.lock() = snapshot;
    }

    /// Force the next `sync_applescript_snapshot` to rebuild and run it now,
    /// so a forgotten pending object stops resolving at once.
    fn resync_applescript_snapshot(&mut self) {
        self.applescript_snapshot_sig = 0;
        self.applescript_snapshot_at = None;
        self.sync_applescript_snapshot();
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
            state.tab_id.hash(&mut hasher);
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

    /// Bring every tab's accessibility elements in line with its panes
    /// (agent-automation REQ-015). Only the visible panes are listed: a
    /// zoomed tab shows just its zoomed pane.
    pub(super) fn sync_accessibility(&mut self) {
        for state in self.windows.values_mut() {
            let mut panes: Vec<crate::macos_accessibility::AxPane> = state
                .surfaces
                .iter()
                .filter(|(pane, _)| state.zoomed.is_none_or(|zoomed| zoomed == **pane))
                .map(|(pane, surface)| crate::macos_accessibility::AxPane {
                    id: pane.get(),
                    rect: surface.rect,
                    focused: state.focused_pane == *pane,
                    terminal: Some(surface.terminal.clone()),
                })
                .collect();
            panes.sort_by_key(|pane| pane.id);
            let scale_factor = state.window.scale_factor();
            state.ax.sync(&state.window, scale_factor, &panes);
        }
    }

    /// The live window holding `pane`.
    fn script_pane_window(&self, pane: PaneId) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|(_, state)| state.contains_pane(pane))
            .map(|(id, _)| *id)
    }

    /// Run every queued scripted mutation, in order (applescript R-11).
    pub(super) fn drain_applescript_actions(&mut self, event_loop: &ActiveEventLoop) {
        loop {
            let Some(action) = self.applescript_shared.actions.lock().pop_front() else {
                break;
            };
            self.run_applescript_action(event_loop, action);
        }
    }

    fn run_applescript_action(&mut self, event_loop: &ActiveEventLoop, action: ScriptAction) {
        match action {
            ScriptAction::SpawnTab {
                group,
                new_group,
                tab_id,
                pane_id,
                config,
            } => self.script_spawn_tab(event_loop, group, new_group, tab_id, pane_id, config),
            ScriptAction::Split {
                target,
                direction,
                pane_id,
                config,
            } => self.script_split(target, direction, pane_id, config),
            ScriptAction::WriteText { pane, text } => {
                let pane_id = PaneId::new(pane);
                let Some(window_id) = self.script_pane_window(pane_id) else {
                    return;
                };
                // Bracketed-paste wrapping follows the pane's mode now, at
                // processing time (applescript Amendment 1.5).
                let bracketed = self.bracketed_paste(window_id, pane_id);
                if let Some(bytes) = input::applescript_input_bytes(&text, bracketed) {
                    self.mark_pane_paste_input(window_id, pane_id);
                    self.snap_pane_viewport_to_bottom(window_id, pane_id);
                    self.write_pane_pty_bytes(window_id, pane_id, bytes);
                }
            }
            ScriptAction::Raise { pane, activate_app } => {
                self.script_raise(PaneId::new(pane), activate_app);
            }
            ScriptAction::ClosePane { pane } => {
                let pane_id = PaneId::new(pane);
                if let Some(window_id) = self.script_pane_window(pane_id) {
                    // A close confirmation shows in its tab only: bring that
                    // tab forward first, or the prompt sits in a hidden tab.
                    if self.pane_has_running_program(window_id, pane_id) {
                        self.script_raise(pane_id, false);
                    }
                    self.request_close_pane(event_loop, window_id, pane_id);
                }
            }
            ScriptAction::CloseTab { pane } => {
                // The whole tab, through the UI's confirm/close path — not
                // `AppCommand::CloseTab`, which closes only the focused pane
                // of a split tab.
                let pane_id = PaneId::new(pane);
                if let Some(window_id) = self.script_pane_window(pane_id) {
                    if self.tab_running_program_count(window_id) > 0 {
                        self.script_raise(pane_id, false);
                    }
                    self.request_close_tab(event_loop, window_id);
                }
            }
            ScriptAction::CloseWindow { pane } => {
                if self.script_raise(PaneId::new(pane), false) {
                    self.handle_app_command(
                        event_loop,
                        AppCommand::CloseWindow,
                        CommandOrigin::App,
                    );
                }
            }
            ScriptAction::Perform { pane, command } => {
                // ASSUME-3: `on <terminal>` focuses the target first; the
                // command then acts on the focused terminal like a keybind.
                if let Some(pane) = pane
                    && !self.script_raise(PaneId::new(pane), false)
                {
                    return;
                }
                self.handle_app_command(event_loop, command, CommandOrigin::App);
            }
            ScriptAction::SendKey {
                pane,
                key,
                mods,
                pressed,
            } => self.script_send_key(event_loop, PaneId::new(pane), key, mods, pressed),
            ScriptAction::SendMouse { pane, mods, event } => {
                self.script_send_mouse(event_loop, PaneId::new(pane), mods, event)
            }
        }
    }

    /// Raise `pane`'s native tab/window and focus it (applescript R-5). Unlike
    /// a plain split-focus this always re-orders the window, and
    /// `activate_app` also brings the whole app forward. `false` when the
    /// pane is gone.
    fn script_raise(&mut self, pane_id: PaneId, activate_app: bool) -> bool {
        let Some(window_id) = self.script_pane_window(pane_id) else {
            return false;
        };
        self.focused = Some(window_id);
        self.focus_pane(window_id, pane_id);
        if let Some(window) = self
            .windows
            .get(&window_id)
            .map(|state| state.window.clone())
        {
            window.focus_window();
        }
        #[cfg(target_os = "macos")]
        if activate_app {
            crate::macos_window::activate_app();
        }
        #[cfg(not(target_os = "macos"))]
        let _ = activate_app;
        true
    }

    fn script_spawn_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        group: u64,
        new_group: bool,
        tab_id: u64,
        pane_id: u64,
        config: SurfaceConfig,
    ) {
        let forget = |app: &mut Self| {
            app.applescript_shared.pending.lock().forget(
                pane_id,
                Some(tab_id),
                new_group.then_some(group),
            );
            app.resync_applescript_snapshot();
        };
        // `new tab in <window>` whose window failed to appear (or closed
        // since): drop, like any action whose target is gone.
        if !new_group && !self.windows.values().any(|state| state.group.0 == group) {
            forget(self);
            return;
        }
        let mut spawn = ScriptSpawn::from_config(&config);
        spawn.pane_id = Some(PaneId::new(pane_id));
        spawn.tab_id = Some(tab_id);
        spawn.group_id = new_group.then_some(group);
        // Without a configured cwd the tab inherits the focused shell's, like
        // an interactive New Tab.
        let cwd = spawn.cwd.clone().or_else(|| self.focused_pane_cwd());
        self.script_spawn = Some(spawn);
        let choice = if new_group {
            GroupChoice::Fresh
        } else {
            GroupChoice::Existing(WindowGroupId(group))
        };
        let result = self.spawn_script_tab(event_loop, choice, cwd);
        self.script_spawn = None;
        let window_id = match result {
            Ok(window_id) => window_id,
            Err(err) => {
                log::warn!("failed to spawn AppleScript tab: {err:#}");
                forget(self);
                return;
            }
        };
        // Retire the promise now: if the tab closes again before the next
        // snapshot rebuild (a short-lived command), the rebuild would never
        // see it live and the pending entry would linger.
        self.applescript_shared.pending.lock().retire(
            pane_id,
            Some(tab_id),
            new_group.then_some(group),
        );
        self.applescript_snapshot_sig = 0;
        // `font size` applies to the tab a new window/tab creates (REQ-004).
        if let Some(points) = config.font_size {
            self.apply_tab_font_size(window_id, script_font_points(points));
        }
        self.write_initial_input(window_id, PaneId::new(pane_id), config.initial_input);
    }

    fn script_split(
        &mut self,
        target: u64,
        direction: ScriptSplit,
        pane_id: u64,
        config: SurfaceConfig,
    ) {
        let new_pane = PaneId::new(pane_id);
        let target = PaneId::new(target);
        let created = match self.script_pane_window(target) {
            Some(window_id) => {
                self.focus_pane(window_id, target);
                let mut spawn = ScriptSpawn::from_config(&config);
                spawn.pane_id = Some(new_pane);
                self.script_spawn = Some(spawn);
                self.new_split(
                    window_id,
                    match direction {
                        ScriptSplit::Right => Direction::Right,
                        ScriptSplit::Left => Direction::Left,
                        ScriptSplit::Down => Direction::Down,
                        ScriptSplit::Up => Direction::Up,
                    },
                );
                self.script_spawn = None;
                self.windows
                    .get(&window_id)
                    .is_some_and(|state| state.contains_pane(new_pane))
                    .then_some(window_id)
            }
            None => None,
        };
        match created {
            // `font size` is ignored for a split: the pane keeps its tab's
            // size (REQ-004).
            Some(window_id) => {
                self.applescript_shared
                    .pending
                    .lock()
                    .retire(pane_id, None, None);
                self.applescript_snapshot_sig = 0;
                self.write_initial_input(window_id, new_pane, config.initial_input);
            }
            None => {
                self.applescript_shared
                    .pending
                    .lock()
                    .forget(pane_id, None, None);
                self.resync_applescript_snapshot();
            }
        }
    }

    /// `initial input`: written raw to the new pane's pty, right after spawn
    /// (L2.4), capped like `input text`.
    fn write_initial_input(&mut self, window_id: WindowId, pane_id: PaneId, text: Option<String>) {
        if let Some(bytes) = text.as_deref().and_then(input::raw_input_bytes) {
            self.write_pane_pty_bytes(window_id, pane_id, bytes);
        }
    }

    /// Re-rasterize one tab at `points` (agent-automation REQ-004): the same
    /// per-window font switch a scale change performs, then a relayout.
    fn apply_tab_font_size(&mut self, window_id: WindowId, points: f32) {
        let Some(scale_factor) = self
            .windows
            .get(&window_id)
            .map(|state| state.window.scale_factor())
        else {
            return;
        };
        let px = font_pixel_size(clamp_runtime_font_size(points), scale_factor);
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        if !gpu.fonts.ensure(px) {
            log::warn!("AppleScript font size {points} pt: no usable font");
            return;
        }
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.font_px = px;
            let atlases = gpu.font_atlases.get(
                &gpu.device,
                &gpu.queue,
                state.surface_config.format,
                gpu.fonts.get(px),
            );
            state.renderer.rebind_glyph_atlases(&gpu.device, &atlases);
            state
                .renderer
                .sync_atlas(&gpu.device, &gpu.queue, gpu.fonts.get_mut(px));
        }
        self.relayout_and_resize_window(window_id);
        self.request_window_redraw(window_id);
    }

    /// `wait after command` (REQ-005): when `pane_id`'s process exits, keep
    /// the pane and print the exit notice instead of closing it. `true` when
    /// the pane was kept.
    pub(super) fn hold_pane_after_exit(&mut self, window_id: WindowId, pane_id: PaneId) -> bool {
        let Some(surface) = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.surfaces.get_mut(&pane_id))
        else {
            return false;
        };
        if !surface.wait_after_command || surface.exited {
            return false;
        }
        surface.exited = true;
        {
            let mut terminal = surface.terminal.lock();
            noa_vt::Stream::new().feed(PROCESS_EXITED_MESSAGE, &mut *terminal);
        }
        self.request_window_redraw(window_id);
        true
    }

    /// The exited pane a key press in `window_id` should close, if the
    /// focused pane is being held open after its process exited.
    pub(super) fn exited_focused_pane(&self, window_id: WindowId) -> Option<PaneId> {
        let state = self.windows.get(&window_id)?;
        state
            .surfaces
            .get(&state.focused_pane)
            .is_some_and(|surface| surface.exited)
            .then_some(state.focused_pane)
    }

    /// `send key` (REQ-010): translate once through the current layout and
    /// encode through the same path as a physical press, with `pane_id`'s own
    /// terminal modes.
    fn script_send_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        pane_id: PaneId,
        key: &'static crate::applescript_keys::ScriptKeyDef,
        mods: ModifiersState,
        pressed: bool,
    ) {
        let Some(window_id) = self.script_pane_window(pane_id) else {
            return;
        };
        let exited = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .is_some_and(|surface| surface.exited);
        if exited {
            if pressed && !key.named.is_some_and(is_modifier_key) && !mods.super_key() {
                self.close_pane_after_pty_exit(event_loop, window_id, pane_id);
            }
            return;
        }
        let option_as_alt = self.option_claims_alt();
        let resolved = crate::applescript_keys::resolve_key(
            key,
            mods,
            option_as_alt,
            crate::applescript_keys::translate_with_current_layout,
        );
        let (app_cursor_keys, app_keypad, kitty_flags, modify_other_keys, backarrow_key_mode) =
            self.windows
                .get(&window_id)
                .and_then(|state| state.surfaces.get(&pane_id))
                .map(|surface| {
                    let terminal = surface.terminal.lock();
                    (
                        terminal.modes.app_cursor_keys(),
                        terminal.modes.keypad_application_keys(),
                        terminal.kitty_keyboard_flags(),
                        terminal.modify_other_keys_2,
                        terminal.modes.backarrow_key_mode(),
                    )
                })
                .unwrap_or((false, false, 0, false, false));
        let bytes = input::encode_key_with_modes(
            &resolved.logical,
            Some(&resolved.unmodified),
            Some(PhysicalKey::Code(resolved.physical)),
            resolved.text.as_deref(),
            mods,
            resolved.alt_sends_esc,
            app_cursor_keys,
            app_keypad,
            kitty_flags,
            modify_other_keys,
            backarrow_key_mode,
            pressed,
            false,
        );
        if let Some(bytes) = bytes {
            self.mark_pane_user_input(window_id, pane_id);
            self.snap_pane_viewport_to_bottom(window_id, pane_id);
            self.write_pane_pty_bytes(window_id, pane_id, bytes);
        }
    }

    /// Whether `macos-option-as-alt` claims a scripted Option (so it
    /// ESC-prefixes rather than composing through the layout). A script's
    /// `option` names no side; like Ghostty's unsided modifier it counts as
    /// the left key.
    fn option_claims_alt(&self) -> bool {
        matches!(
            self.config.macos_option_as_alt,
            noa_config::MacosOptionAsAlt::Left | noa_config::MacosOptionAsAlt::Both
        )
    }

    /// `send mouse …` (REQ-011): replay the event through the same handlers
    /// a physical pointer event runs, positioned inside `pane_id`. The system
    /// pointer never moves, and the real pointer's state is put back after,
    /// so the user's next click or wheel still goes where their cursor is.
    fn script_send_mouse(
        &mut self,
        event_loop: &ActiveEventLoop,
        pane_id: PaneId,
        mods: ModifiersState,
        event: ScriptMouse,
    ) {
        let Some(window_id) = self.script_pane_window(pane_id) else {
            return;
        };
        let Some((rect, scale_factor, real_pointer, saved_pointer, real_state)) =
            self.windows.get(&window_id).and_then(|state| {
                let surface = state.surfaces.get(&pane_id)?;
                // The real pointer counts only while it is over this pane with
                // a cell there (a fresh tab or split names the pane as
                // `last_mouse_pane` before any motion gave it a cell) and no
                // scripted position in this pane has superseded it.
                let real = (state.last_mouse_pane == Some(pane_id)
                    && surface.last_mouse_cell.is_some()
                    && !surface.script_pointer_live)
                    .then_some(state.last_mouse_physical_position)
                    .flatten();
                let real_state = (
                    state.last_mouse_point,
                    state.last_mouse_physical_position,
                    state.last_mouse_pane,
                    surface.last_mouse_cell,
                    surface.last_mouse_pixel,
                );
                Some((
                    surface.rect,
                    state.window.scale_factor(),
                    real,
                    surface.script_pointer,
                    real_state,
                ))
            })
        else {
            return;
        };
        let saved_modifiers = self.modifiers;
        self.modifiers = mods;
        // Route every event of this delivery to `pane_id` itself.
        self.script_mouse_pane = Some(pane_id);
        // A button or wheel event lands where the pane's pointer is, placed
        // against the current layout (a resize or font change leaves a cached
        // cell stale) without a motion event, since nothing moved: the real
        // pointer when it is over this pane, else this pane's last scripted
        // position — kept pane-relative and re-mapped onto the pane's current
        // rect — or, never positioned, its top-left.
        let press_position = script_press_position(real_pointer, saved_pointer, rect, scale_factor);
        match event {
            ScriptMouse::Position { x, y } => {
                if let Some(state) = self.windows.get_mut(&window_id)
                    && let Some(surface) = state.surfaces.get_mut(&pane_id)
                {
                    surface.script_pointer = Some((x, y));
                    surface.script_pointer_live = true;
                }
                let position = script_point_to_physical(rect, scale_factor, x, y);
                self.on_cursor_moved(window_id, position);
            }
            ScriptMouse::Button { button, pressed } => {
                self.script_place_pointer(window_id, pane_id, press_position);
                let state = if pressed {
                    ElementState::Pressed
                } else {
                    ElementState::Released
                };
                self.on_mouse_input(event_loop, window_id, state, button);
            }
            ScriptMouse::Scroll { dx, dy, precise } => {
                self.script_place_pointer(window_id, pane_id, press_position);
                let delta = script_scroll_delta(dx, dy, precise, scale_factor);
                self.on_mouse_wheel(window_id, delta);
            }
        }
        self.script_mouse_pane = None;
        self.modifiers = saved_modifiers;
        let (point, physical, pane, cell, pixel) = real_state;
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.last_mouse_point = point;
            state.last_mouse_physical_position = physical;
            state.last_mouse_pane = pane;
            // `reported_cell` keeps what the terminal last saw.
            if let Some(surface) = state.surfaces.get_mut(&pane_id) {
                surface.last_mouse_cell = cell;
                surface.last_mouse_pixel = pixel;
            }
        }
        self.update_sidebar_button_hover(window_id, point);
        self.sync_hover_link(window_id);
    }

    /// Put `window_id`'s pointer state at `position` inside `pane_id` — the
    /// cell, pixel and hover a pointer there has — without reporting motion.
    fn script_place_pointer(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        position: PhysicalPosition<f64>,
    ) {
        let font_px = self.window_font_px(window_id);
        let Some(metrics) = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.fonts.get(font_px).metrics())
        else {
            return;
        };
        let cell = self.pane_cell_in(window_id, pane_id, position, metrics);
        let pixel = self.pane_pixel_in(window_id, pane_id, position);
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.last_mouse_point = split_point_from_physical_position(position);
            state.last_mouse_physical_position = Some(position);
            state.last_mouse_pane = Some(pane_id);
            if let Some(surface) = state.surfaces.get_mut(&pane_id) {
                surface.last_mouse_cell = cell;
                surface.last_mouse_pixel = pixel;
            }
        }
        self.sync_hover_link(window_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted point maps to exactly the cell a pointer there hits, at
    /// any cell size and padding: nothing nudges it inward (a 6 pt font at
    /// scale 1 has 4 px cells — narrower than a divider's hit zone).
    /// A saved scripted position follows the pane's current geometry: after
    /// the pane shrinks and the scale changes it stays pane-relative and
    /// inside the new rect, while a real pointer over the pane wins.
    #[test]
    fn held_pane_close_key_swallows_its_repeats_and_release_only() {
        use winit::keyboard::KeyCode;
        let enter = PhysicalKey::Code(KeyCode::Enter);
        let other = PhysicalKey::Code(KeyCode::KeyA);
        let mut keys = HashSet::from([enter]);
        assert!(!swallow_held_pane_close_key(&mut keys, other, true, false));
        assert!(swallow_held_pane_close_key(&mut keys, enter, true, true));
        assert!(swallow_held_pane_close_key(&mut keys, enter, true, true));
        assert!(swallow_held_pane_close_key(&mut keys, enter, false, false));
        // Released: the next press of the key is ordinary input again.
        assert!(!swallow_held_pane_close_key(&mut keys, enter, true, false));
        // A lost release: a fresh press retires the key and goes through.
        let mut keys = HashSet::from([enter]);
        assert!(!swallow_held_pane_close_key(&mut keys, enter, true, false));
        assert!(keys.is_empty());
    }

    #[test]
    fn script_press_position_remaps_the_saved_point_onto_the_current_rect() {
        let before = PaneRectApp::new(100, 50, 800, 600);
        let after = PaneRectApp::new(10, 20, 200, 100);
        let saved = Some((150.0, 40.0));
        assert_eq!(
            script_press_position(None, saved, before, 1.0),
            PhysicalPosition::new(250.0, 90.0)
        );
        assert_eq!(
            script_press_position(None, saved, after, 2.0),
            PhysicalPosition::new(10.0 + 199.0, 20.0 + 80.0)
        );
        assert_eq!(
            script_press_position(None, None, after, 2.0),
            PhysicalPosition::new(10.0, 20.0)
        );
        let real = PhysicalPosition::new(42.0, 43.0);
        assert_eq!(script_press_position(Some(real), saved, after, 2.0), real);
    }

    #[test]
    fn script_points_keep_their_cell_with_small_cells() {
        let grid = GridSize::new(80, 24);
        let padding = GridPadding {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        };
        let rect = PaneRectApp::new(801, 0, 80 * 4, 24 * 8);
        let cell = |x: f64, y: f64| {
            let pos = script_point_to_physical(rect, 1.0, x, y);
            let point = mouse::physical_position_to_grid_point(
                pos.x - f64::from(rect.x),
                pos.y - f64::from(rect.y),
                4.0,
                8.0,
                grid,
                padding,
            );
            (point.x + 1, point.y + 1)
        };
        assert_eq!(cell(0.0, 0.0), (1, 1));
        assert_eq!(cell(4.0, 8.0), (2, 2));
        assert_eq!(cell(10_000.0, 10_000.0), (80, 24));
        // Kept pane-relative: the same point follows the pane when it moves.
        let moved = PaneRectApp::new(0, 0, 80 * 4, 24 * 8);
        assert_eq!(
            script_point_to_physical(moved, 1.0, 6.0, 9.0),
            PhysicalPosition::new(6.0, 9.0)
        );
    }

    #[test]
    fn script_scroll_delta_is_bounded() {
        assert_eq!(
            script_scroll_delta(0.0, 1.9, false, 2.0),
            MouseScrollDelta::LineDelta(0.0, 1.0)
        );
        assert_eq!(
            script_scroll_delta(-1e20, 1e20, false, 2.0),
            MouseScrollDelta::LineDelta(-1_000.0, 1_000.0)
        );
        assert_eq!(
            script_scroll_delta(1.5, -1e20, true, 2.0),
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(3.0, -200_000.0))
        );
    }

    #[test]
    fn invalid_cwd_falls_back() {
        let dir = std::env::temp_dir();
        let dir_str = dir.to_string_lossy().into_owned();
        assert_eq!(usable_cwd(Some(dir_str.clone())), Some(dir_str));
        assert_eq!(usable_cwd(Some("/no/such/noa-dir".into())), None);
        // A file is not a directory.
        let file = dir.join(format!("noa-cwd-probe-{}", std::process::id()));
        std::fs::write(&file, b"").expect("probe file");
        assert_eq!(usable_cwd(Some(file.to_string_lossy().into_owned())), None);
        let _ = std::fs::remove_file(&file);
        assert_eq!(usable_cwd(None), None);
    }

    #[test]
    fn script_font_size_clamps_before_narrowing() {
        let min = MIN_RUNTIME_FONT_SIZE;
        let max = MAX_RUNTIME_FONT_SIZE;
        assert_eq!(script_font_points(-1.0), min);
        assert_eq!(script_font_points(1.0), min);
        assert_eq!(script_font_points(14.0), 14.0);
        assert_eq!(script_font_points(1e300), max);
        assert_eq!(script_font_points(-1e300), min);
        assert_eq!(script_font_points(f64::from(max)), max);
    }

    #[test]
    fn ime_typing_counts_as_a_key_press() {
        assert!(ime_event_types_text(&Ime::Preedit(
            "あ".into(),
            Some((0, 3))
        )));
        assert!(ime_event_types_text(&Ime::Commit("漢字".into())));
        // Clearing a composition or toggling the IME is not typing.
        assert!(!ime_event_types_text(&Ime::Preedit(String::new(), None)));
        assert!(!ime_event_types_text(&Ime::Commit(String::new())));
        assert!(!ime_event_types_text(&Ime::Enabled));
        assert!(!ime_event_types_text(&Ime::Disabled));
    }

    #[test]
    fn held_pane_ignores_modifiers_and_cmd_shortcuts() {
        let none = ModifiersState::empty();
        assert!(closes_held_pane_exempt(&Key::Named(NamedKey::Shift), none));
        assert!(closes_held_pane_exempt(
            &Key::Character("c".into()),
            ModifiersState::SUPER
        ));
        assert!(!closes_held_pane_exempt(&Key::Character("a".into()), none));
        assert!(!closes_held_pane_exempt(&Key::Named(NamedKey::Enter), none));
    }

    /// AC-032: a script point maps to the cell a pointer there reports
    /// (1-based `floor((p - padding) / cell) + 1`, clamped to the grid).
    #[test]
    fn applescript_mouse_cell_mapping() {
        let grid = GridSize::new(80, 24);
        let padding = GridPadding {
            left: 4.0,
            top: 2.0,
            right: 4.0,
            bottom: 2.0,
        };
        let rect = PaneRectApp::new(0, 0, 4 + 80 * 8 + 4, 2 + 24 * 16 + 2);
        let cell = |x: f64, y: f64| {
            let pos = script_point_to_physical(rect, 1.0, x, y);
            let point =
                mouse::physical_position_to_grid_point(pos.x, pos.y, 8.0, 16.0, grid, padding);
            (point.x + 1, point.y + 1)
        };
        assert_eq!(cell(300.0, 120.0), (38, 8));
        assert_eq!(cell(0.0, 0.0), (1, 1));
        assert_eq!(cell(10_000.0, 10_000.0), (80, 24));
        // Retina: points scale to physical pixels before the cell lookup.
        let retina = PaneRectApp::new(0, 0, 2 * (4 + 80 * 8 + 4), 2 * (2 + 24 * 16 + 2));
        let pos = script_point_to_physical(retina, 2.0, 300.0, 120.0);
        let padding2 = GridPadding {
            left: 8.0,
            top: 4.0,
            right: 8.0,
            bottom: 4.0,
        };
        let point =
            mouse::physical_position_to_grid_point(pos.x, pos.y, 16.0, 32.0, grid, padding2);
        assert_eq!((point.x + 1, point.y + 1), (38, 8));
    }
}
