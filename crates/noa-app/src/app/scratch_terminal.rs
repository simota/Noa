//! Scratch terminal: a single-instance, use-once-and-discard popup terminal
//! (`docs/specs/scratch-terminal.md`). Deliberately simpler than
//! `quick_terminal.rs`: no animation, no persistence across toggles, no
//! global hotkey, no target-screen resolution — every toggle spawns fresh
//! and every close tears the whole thing down (pty, window, surface).

use super::*;

/// Runtime state for the scratch terminal popup. Unlike
/// [`super::quick_terminal::QuickTerminalState`] there is no separate
/// hidden/visible axis: `App::scratch_terminal` being `Some` *is* "shown"
/// (R4/R5) — the window itself lives in `windows` (so it reuses the whole
/// redraw/input/resize path) but is deliberately kept out of `window_order`.
pub(super) struct ScratchTerminalState {
    pub(super) window_id: WindowId,
    /// The window the popup was spawned centered on (R2) — refocused on a
    /// non-focus-loss destroy (fix 1c), falling back to `window_order.last()`
    /// if it has since closed.
    anchor_window_id: WindowId,
    /// Whether this popup has ever received a real OS focus-gain event.
    /// macOS can emit a stale `Focused(false)` while a borderless window is
    /// still being ordered front (the same hazard `QuickTerminalState`'s
    /// `focused_this_reveal` guards); gating on this prevents the popup from
    /// destroying itself the instant it appears (R4-b/AC-13).
    focused_once: bool,
}

/// Window/tab-management commands that are no-ops while the scratch
/// terminal popup is focused (R6-iii): they all operate on tab/split/window
/// topology the single-pane, `window_order`-excluded popup doesn't have.
/// `CloseTab` (⌘W) is handled separately — it closes the popup (R4-d) rather
/// than being blocked or falling through to the normal tab-close path.
/// Everything not listed here (copy/paste/clear/select-all/font-size, and
/// anything else not about tab/window topology — R6-ii) passes through
/// unchanged.
pub(super) fn scratch_terminal_command_blocked(command: AppCommand) -> bool {
    matches!(
        command,
        AppCommand::NewTab
            | AppCommand::NewWindow
            | AppCommand::NewSplitLeft
            | AppCommand::NewSplitRight
            | AppCommand::NewSplitUp
            | AppCommand::NewSplitDown
            | AppCommand::FocusDirection(_)
            | AppCommand::ResizeSplit(_)
            | AppCommand::EqualizeSplits
            | AppCommand::ToggleSplitZoom
            | AppCommand::ToggleTabOverview
            | AppCommand::SelectTab(_)
            | AppCommand::NextTab
            | AppCommand::PrevTab
            | AppCommand::SetTabTitle
            | AppCommand::CloseWindow
            | AppCommand::ToggleSidebar
            // Fix 3: `ToggleFullscreen` acts on the focused *window* (its
            // native/borderless-fullscreen chrome), and
            // `PipeScrollbackToPager` spawns a new tab/pager pane — both are
            // window/tab-topology operations on a popup that has neither
            // real chrome nor a tab of its own.
            | AppCommand::ToggleFullscreen
            | AppCommand::PipeScrollbackToPager
    )
}

/// The scratch popup's target grid size in physical px for `cols`x`rows`
/// cells at `metrics`/`padding`, clamped to at most `max_fraction` of
/// `anchor_inner` (AC-10: "≤90% of the focused window's inner size").
/// Pure/testable — the impure lookups (font metrics, anchor window size)
/// happen in [`App::spawn_scratch_terminal`].
pub(super) fn scratch_terminal_footprint_px(
    cols: u16,
    rows: u16,
    metrics: noa_font::Metrics,
    padding: GridPadding,
    anchor_inner: (u32, u32),
    max_fraction: f32,
) -> (u32, u32) {
    let width = padding.horizontal() + cols as f32 * metrics.cell_w.max(f32::EPSILON);
    let height = padding.vertical() + rows as f32 * metrics.cell_h.max(f32::EPSILON);
    let max_width = (anchor_inner.0 as f32 * max_fraction).max(1.0);
    let max_height = (anchor_inner.1 as f32 * max_fraction).max(1.0);
    (
        width.round().clamp(1.0, max_width) as u32,
        height.round().clamp(1.0, max_height) as u32,
    )
}

/// The popup's top-left origin (physical px), centered on the anchor
/// window's outer frame (R2/L2: "フォーカス中ウィンドウの outer frame 中央").
pub(super) fn scratch_terminal_origin(
    anchor_outer_origin: (i32, i32),
    anchor_outer_size: (u32, u32),
    popup_size: (u32, u32),
) -> (i32, i32) {
    (
        anchor_outer_origin.0 + (anchor_outer_size.0 as i32 - popup_size.0 as i32) / 2,
        anchor_outer_origin.1 + (anchor_outer_size.1 as i32 - popup_size.1 as i32) / 2,
    )
}

/// scratch-terminal R3: the cwd a freshly spawned popup should inherit.
/// `focused_pane_cwd` is already a live check (`pane_cwd`'s `Path::is_dir`
/// guard runs at call time, not from a cache), so this is a direct
/// passthrough — kept as a named seam so the fallback contract (AC-4: a
/// `None` cwd must not crash, and falls back to the process's own cwd via
/// `PtyConfig`'s default) is documented and independently testable.
pub(super) fn scratch_terminal_spawn_cwd(focused_pane_cwd: Option<String>) -> Option<String> {
    focused_pane_cwd
}

/// Fraction of the focused window's inner size the popup may occupy at most
/// (R2/AC-10).
const SCRATCH_TERMINAL_MAX_FRACTION: f32 = 0.9;

impl App {
    pub(super) fn is_scratch_terminal_window(&self, window_id: WindowId) -> bool {
        self.scratch_terminal
            .as_ref()
            .is_some_and(|state| state.window_id == window_id)
    }

    /// Toggle the scratch terminal: spawn+show when absent, destroy when
    /// present (R5: single instance, enforced by the `Option` type
    /// invariant — every path below checks `is_some()`/`is_none()` first, so
    /// a rapid double-toggle can only ever spawn once before the first
    /// toggle's destroy would need to run first). A no-op before the GPU
    /// exists, mirroring the quick terminal.
    pub(super) fn toggle_scratch_terminal(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_none() {
            return;
        }
        if self.scratch_terminal.is_some() {
            self.destroy_scratch_terminal();
        } else {
            self.spawn_scratch_terminal(event_loop);
        }
    }

    /// Spawn the popup centered on the focused window, or no-op when there
    /// is no eligible anchor (no window focused yet, or the only window is
    /// itself a quick-terminal/scratch popup).
    fn spawn_scratch_terminal(&mut self, event_loop: &ActiveEventLoop) {
        let toggle_pressed_at = Instant::now();
        let Some(anchor_window_id) = self.focused.filter(|id| {
            self.windows.contains_key(id)
                && !self.is_quick_terminal_window(*id)
                && !self.is_scratch_terminal_window(*id)
        }) else {
            return;
        };
        let Some(anchor) = self.windows.get(&anchor_window_id) else {
            return;
        };
        let anchor_window = anchor.window.clone();
        let Ok(anchor_outer_position) = anchor_window.outer_position() else {
            return;
        };
        let anchor_outer_size = anchor_window.outer_size();
        let anchor_inner_size = anchor_window.inner_size();
        let scale_factor = anchor_window.scale_factor();

        let cwd = scratch_terminal_spawn_cwd(self.focused_pane_cwd());

        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let metrics = gpu.font.metrics();
        let (width, height) = scratch_terminal_footprint_px(
            self.config.scratch_terminal_size.cols,
            self.config.scratch_terminal_size.rows,
            metrics,
            self.padding,
            (anchor_inner_size.width, anchor_inner_size.height),
            SCRATCH_TERMINAL_MAX_FRACTION,
        );
        let (origin_x, origin_y) = scratch_terminal_origin(
            (anchor_outer_position.x, anchor_outer_position.y),
            (anchor_outer_size.width, anchor_outer_size.height),
            (width, height),
        );

        let attrs = WindowAttributes::default()
            .with_title("Scratch Terminal")
            .with_decorations(false)
            .with_inner_size(PhysicalSize::new(width, height))
            .with_position(PhysicalPosition::new(origin_x, origin_y))
            .with_transparent(self.config.background_opacity < 1.0)
            .with_visible(false);
        #[cfg(target_os = "macos")]
        let attrs = attrs.with_option_as_alt(macos_option_as_alt(self.config.macos_option_as_alt));
        let Ok(window) = event_loop.create_window(attrs) else {
            return;
        };
        let window = Arc::new(window);
        window.set_ime_allowed(true);
        crate::macos_blur::apply_background_blur(
            &window,
            self.config.background_blur_radius,
            self.config.background_opacity,
        );
        // Same borderless/floating/all-Spaces AppKit configuration as the
        // quick terminal — generic window dressing, not QT-specific despite
        // the function's name (L2).
        crate::macos_window::configure_quick_terminal_window(&window);

        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Ok(surface) = gpu.instance.create_surface(window.clone()) else {
            return;
        };
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let caps = surface.get_capabilities(&gpu.adapter);
        let alpha_blending = alpha_blending_mode(&self.config.font);
        let surface_format = preferred_surface_format(&caps.formats, alpha_blending);
        let size = window.inner_size();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 1,
            alpha_mode: preferred_surface_alpha_mode(&caps, self.config.background_opacity < 1.0),
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &surface_config);
        let pipelines = gpu.pipelines.get(&gpu.device, surface_format);
        let font_atlases = gpu
            .font_atlases
            .get(&gpu.device, &gpu.queue, surface_format, &gpu.font);
        let Ok(mut renderer) = Renderer::with_pipelines(
            &gpu.device,
            &gpu.queue,
            &pipelines,
            &font_atlases,
            &mut gpu.font,
            self.padding,
        ) else {
            return;
        };
        renderer.set_background_opacity(self.config.background_opacity);
        renderer.set_alpha_blending(alpha_blending);
        renderer.set_background_image(&gpu.device, &gpu.queue, self.background_image.current_image());
        renderer.resize(PixelSize {
            w: surface_config.width,
            h: surface_config.height,
        });

        let window_id = window.id();
        let initial_pane = PaneId::alloc();
        let initial_rect = PaneRectApp::new(0, 0, surface_config.width, surface_config.height);
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let metrics = gpu.font.metrics();
        let grid = grid_size_for_pane_rect(initial_rect, metrics, self.padding);
        let auto_approve_enabled = Arc::new(AtomicBool::new(false));
        let redraw_floor = crate::io_thread::RedrawFloor::new(
            crate::io_thread::redraw_floor_from_refresh_millihertz(
                window
                    .current_monitor()
                    .and_then(|monitor| monitor.refresh_rate_millihertz()),
            ),
        );
        let Ok(initial_surface) = self.spawn_pane_surface(
            window_id,
            initial_pane,
            grid,
            initial_rect,
            cwd,
            auto_approve_enabled.clone(),
            redraw_floor.clone(),
        ) else {
            return;
        };
        let mut surfaces = HashMap::new();
        surfaces.insert(initial_pane, initial_surface);
        let group = self.allocate_group_id();
        self.windows.insert(
            window_id,
            WindowState {
                window: window.clone(),
                group,
                surface,
                surface_config,
                renderer,
                split_tree: SplitTree::leaf(initial_pane),
                zoomed: None,
                focused_pane: initial_pane,
                surfaces,
                last_mouse_pane: Some(initial_pane),
                last_mouse_point: None,
                last_mouse_physical_position: None,
                active_split_drag: None,
                occluded: false,
                title: "Scratch Terminal".to_string(),
                title_override: None,
                proxy_icon_cwd: None,
                last_touchpad_stage: 0,
                auto_approve_enabled,
                redraw_floor,
                sidebar_scroll: 0,
                sidebar_button_hover: false,
                sidebar_card_hover: None,
                sidebar_menu: None,
                sidebar_drag: None,
                link_click_in_flight: false,
                file_drop: FileDropState::default(),
                resize_throttle: crate::debounce::Throttle::new(RESIZE_REFLOW_THROTTLE_INTERVAL),
                last_grid: None,
                resize_overlay: None,
                bell_flash_until: None,
                native_overlays: Default::default(),
                applied_window_bg: None,
                bg_refresh_last: None,
                reveal_fast_path_pending: false,
            },
        );
        self.relayout_and_resize_window(window_id);
        self.scratch_terminal = Some(ScratchTerminalState {
            window_id,
            anchor_window_id,
            focused_once: false,
        });

        // NR1/AC-14: pre-paint before the window is ever ordered front (same
        // RC1/RC2 hazard the quick terminal guards against), then show +
        // make key.
        self.redraw(window_id);
        if let Some(state) = self.windows.get(&window_id) {
            state.window.set_visible(true);
            crate::macos_window::show_quick_terminal_window(&state.window);
        }
        self.focused = Some(window_id);
        log::debug!(
            "scratch-terminal: toggle-press to first-frame {:?} (scale factor {scale_factor})",
            toggle_pressed_at.elapsed()
        );
    }

    /// Mark the popup as having received a real focus-gain event, so a
    /// subsequent focus loss is trusted as a real close signal (R4-b).
    pub(super) fn mark_scratch_terminal_focused(&mut self, window_id: WindowId) {
        if let Some(state) = self.scratch_terminal.as_mut()
            && state.window_id == window_id
        {
            state.focused_once = true;
        }
    }

    /// Whether the popup should be destroyed for losing focus (R4-b):
    /// mirrors the quick terminal's `focused_this_reveal` guard against a
    /// stale `Focused(false)` arriving before the real focus-gain event for
    /// a just-shown borderless window. Routes through the no-refocus variant
    /// of destroy (fix 1): the user just moved focus elsewhere on their own,
    /// so stealing it back to the anchor window would fight them — mirrors
    /// the quick terminal's `app_is_active` guard on its own hide-triggered
    /// focus restore (`quick_terminal.rs`'s `start_quick_terminal_hide`).
    pub(super) fn maybe_autoclose_scratch_terminal(&mut self, window_id: WindowId) {
        if self
            .scratch_terminal
            .as_ref()
            .is_some_and(|state| state.window_id == window_id && state.focused_once)
        {
            self.destroy_scratch_terminal_impl(false);
        }
    }

    /// Tear the scratch terminal down outright: pty, window, surface, and
    /// every window-bound overlay session pinned to it, immediately, no
    /// warning (R4). A no-op when nothing is shown. Refocuses the anchor
    /// window (fix 1c) unlike the focus-loss path
    /// ([`Self::maybe_autoclose_scratch_terminal`]).
    pub(super) fn destroy_scratch_terminal(&mut self) {
        self.destroy_scratch_terminal_impl(true);
    }

    /// Shared teardown for both destroy paths (fix 1/2). `refocus_anchor`
    /// distinguishes them:
    /// - `true` (toggle-key close, ⌘W, shell exit, config reload, quit): the
    ///   popup was the active surface the user just dismissed, so focus
    ///   returns to the window that spawned it (falling back to the last
    ///   tracked window if the anchor itself is already gone).
    /// - `false` (the popup's own focus-loss, R4-b): the user has already
    ///   moved focus elsewhere on their own — to another noa window or a
    ///   different app entirely — so this must not steal it back. `self.focused`
    ///   is only cleared if it still (stalely) points at the just-destroyed
    ///   popup; winit's imminent `Focused(true)` for whatever the user
    ///   actually clicked overwrites it a moment later regardless.
    fn destroy_scratch_terminal_impl(&mut self, refocus_anchor: bool) {
        let Some(state) = self.scratch_terminal.take() else {
            return;
        };
        self.end_copy_mode_for_window(state.window_id);
        self.clear_window_bound_overlays(state.window_id);
        let closing_panes: Vec<_> = self
            .windows
            .get(&state.window_id)
            .map(|window_state| window_state.surfaces.keys().copied().collect())
            .unwrap_or_default();
        for pane_id in closing_panes {
            self.cleanup_ipc_attach_pane(state.window_id, pane_id);
        }
        if let Some(mut window_state) = self.windows.remove(&state.window_id) {
            window_state.shutdown();
        }
        if self.focused != Some(state.window_id) {
            // The popup wasn't the focused window at all (e.g. the toggle
            // chord was pressed from the anchor window while the popup sat
            // unfocused) — nothing to fix up.
            return;
        }
        if !refocus_anchor {
            self.focused = None;
            return;
        }
        self.focused = if self.windows.contains_key(&state.anchor_window_id) {
            Some(state.anchor_window_id)
        } else {
            self.window_order.last().copied()
        };
        if let Some(window_id) = self.focused
            && let Some(window_state) = self.windows.get(&window_id)
        {
            window_state.window.focus_window();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(cell_w: f32, cell_h: f32) -> noa_font::Metrics {
        noa_font::Metrics {
            cell_w,
            cell_h,
            ascent: 0.0,
            descent: 0.0,
            line_gap: 0.0,
            underline_position: 0.0,
            underline_thickness: 0.0,
        }
    }

    // AC-10: default 100x25 fits comfortably inside a generous window, so no
    // clamping kicks in — the footprint is exactly cells * cell metrics (plus
    // padding).
    #[test]
    fn footprint_matches_cell_metrics_when_unclamped() {
        let (w, h) = scratch_terminal_footprint_px(
            100,
            25,
            metrics(10.0, 20.0),
            GridPadding::new(0.0, 0.0, 0.0, 0.0),
            (4000, 4000),
            0.9,
        );
        assert_eq!((w, h), (1000, 500));
    }

    // AC-10: an 800x600 window clamps the popup to at most 90% of its inner
    // size on the axis that would otherwise overflow.
    #[test]
    fn footprint_clamps_to_ninety_percent_of_anchor_inner_size() {
        let (w, h) = scratch_terminal_footprint_px(
            100,
            25,
            metrics(10.0, 20.0),
            GridPadding::new(0.0, 0.0, 0.0, 0.0),
            (800, 600),
            0.9,
        );
        assert_eq!(w, 720); // 800 * 0.9
        assert_eq!(h, 500); // 25 * 20.0 = 500, under 600 * 0.9 = 540
    }

    #[test]
    fn origin_centers_on_anchor_outer_frame() {
        let origin = scratch_terminal_origin((100, 200), (800, 600), (400, 300));
        assert_eq!(origin, (100 + 200, 200 + 150));
    }

    // AC-4: a `None` cwd (unreported/vanished dir) passes through unchanged
    // rather than being coerced into some default path — `PtyConfig`'s own
    // default is what actually falls back to the process cwd.
    #[test]
    fn spawn_cwd_passes_through_none_for_process_cwd_fallback() {
        assert_eq!(scratch_terminal_spawn_cwd(None), None);
    }

    #[test]
    fn spawn_cwd_passes_through_live_cwd() {
        assert_eq!(
            scratch_terminal_spawn_cwd(Some("/tmp".to_string())),
            Some("/tmp".to_string())
        );
    }

    // R6: window/tab management commands are blocked; terminal-level
    // commands and the toggle/close itself are not.
    #[test]
    fn window_and_tab_management_commands_are_blocked() {
        for command in [
            AppCommand::NewTab,
            AppCommand::NewWindow,
            AppCommand::NewSplitLeft,
            AppCommand::ToggleTabOverview,
            AppCommand::SelectTab(1),
            AppCommand::NextTab,
            AppCommand::PrevTab,
            AppCommand::SetTabTitle,
            AppCommand::CloseWindow,
            AppCommand::ToggleSidebar,
            AppCommand::ToggleFullscreen,
            AppCommand::PipeScrollbackToPager,
        ] {
            assert!(
                scratch_terminal_command_blocked(command),
                "{command:?} should be blocked"
            );
        }
    }

    #[test]
    fn terminal_and_toggle_commands_are_not_blocked() {
        for command in [
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::Terminal(TerminalAction::Clear),
            AppCommand::Terminal(TerminalAction::SelectAll),
            AppCommand::FontSize(FontSizeAction::Increase),
            AppCommand::ToggleScratchTerminal,
            AppCommand::CloseTab,
            AppCommand::Quit,
        ] {
            assert!(
                !scratch_terminal_command_blocked(command),
                "{command:?} should not be blocked"
            );
        }
    }
}
