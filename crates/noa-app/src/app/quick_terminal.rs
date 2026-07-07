use super::*;

/// How long the quick terminal takes to slide fully in or out — the shared
/// screen-scale duration.
const QUICK_TERMINAL_SLIDE_DURATION: Duration = crate::anim::DUR_SLOW;
/// The quick terminal repaints/repositions at roughly this cadence while
/// sliding (approx. 60 fps), driven off the `about_to_wait` `WaitUntil` timer.
const QUICK_TERMINAL_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Runtime state for the drop-down quick terminal. The window itself is a
/// normal [`WindowState`] entry in `App::windows`; this tracks the slide
/// geometry (physical px, relative to the target monitor) and animation.
pub(super) struct QuickTerminalState {
    pub(super) window_id: WindowId,
    /// Left edge of the panel (monitor origin x).
    origin_x: i32,
    /// The monitor's top edge — the panel's fully-revealed top.
    top_y: i32,
    /// Panel width and height in physical pixels.
    width: u32,
    height: u32,
    /// Whether the panel is revealed (or animating toward revealed). When
    /// `false` and `anim` is `None`, the window is hidden.
    visible: bool,
    /// The in-flight slide, if any.
    anim: Option<QuickTerminalAnim>,
}

/// One in-flight quick-terminal slide.
#[derive(Clone, Copy)]
struct QuickTerminalAnim {
    start: Instant,
    /// Current reveal fraction at `start` (0 = hidden, 1 = fully shown).
    from_reveal: f32,
    /// Target reveal fraction for this slide.
    to_reveal: f32,
}

impl QuickTerminalAnim {
    fn new(start: Instant, from_reveal: f32, to_reveal: f32) -> Self {
        Self {
            start,
            from_reveal: from_reveal.clamp(0.0, 1.0),
            to_reveal: to_reveal.clamp(0.0, 1.0),
        }
    }

    fn reveal_at(self, now: Instant) -> f32 {
        quick_terminal_slide_reveal(
            self.from_reveal,
            self.to_reveal,
            now.duration_since(self.start),
            QUICK_TERMINAL_SLIDE_DURATION,
        )
    }

    fn done(self, now: Instant) -> bool {
        quick_terminal_progress(
            now.duration_since(self.start),
            QUICK_TERMINAL_SLIDE_DURATION,
        ) >= 1.0
    }

    fn hides(self) -> bool {
        self.to_reveal <= 0.0
    }
}

impl QuickTerminalState {
    fn current_reveal(&self, now: Instant) -> f32 {
        if let Some(anim) = self.anim {
            return anim.reveal_at(now);
        }
        if self.visible { 1.0 } else { 0.0 }
    }
}

/// The shared house easing curve (see [`crate::anim`]), re-exported for the
/// slide math and its tests.
pub(super) use crate::anim::ease_out_cubic;
/// Linear slide progress (`0.0..=1.0`) for `elapsed` of `duration`.
pub(super) use crate::anim::linear_progress as quick_terminal_progress;

/// The panel's top edge in physical px relative to the monitor top, for a
/// slide `progress` in `0.0..=1.0` (0 = fully hidden above the screen, 1 =
/// fully revealed). `height` is the panel height in px.
#[cfg(test)]
pub(super) fn quick_terminal_top_offset(height: f32, progress: f32) -> f32 {
    quick_terminal_reveal_top_offset(height, ease_out_cubic(progress))
}

/// The panel's top edge offset for an already-eased reveal fraction.
pub(super) fn quick_terminal_reveal_top_offset(height: f32, reveal: f32) -> f32 {
    -height * (1.0 - reveal.clamp(0.0, 1.0))
}

/// Eased reveal fraction between the current and target reveal states. This
/// keeps interrupted show/hide transitions moving from their current position
/// instead of snapping back to an endpoint.
pub(super) fn quick_terminal_slide_reveal(
    from_reveal: f32,
    to_reveal: f32,
    elapsed: Duration,
    duration: Duration,
) -> f32 {
    crate::anim::lerp(
        from_reveal.clamp(0.0, 1.0),
        to_reveal.clamp(0.0, 1.0),
        ease_out_cubic(quick_terminal_progress(elapsed, duration)),
    )
}

/// The panel height in physical px for a screen `screen_height` px tall and a
/// `size` fraction, clamped to at least one row's worth of pixels.
pub(super) fn quick_terminal_height(screen_height: u32, size: f32) -> u32 {
    let raw = (screen_height as f32 * size.clamp(0.05, 1.0)).round() as u32;
    raw.clamp(1, screen_height.max(1))
}

/// Quick terminal (drop-down) support.
impl App {
    pub(super) fn is_quick_terminal_window(&self, window_id: WindowId) -> bool {
        self.quick_terminal
            .as_ref()
            .is_some_and(|qt| qt.window_id == window_id)
    }

    /// Register the global `quick-terminal-hotkey` and `sidebar-hotkey` once,
    /// after the app is running. A no-op per chord when unset or explicitly
    /// disabled; a registration failure is logged, not fatal. Both go through
    /// the same `parse_hotkey` path (FR-13).
    pub(super) fn install_global_hotkey_if_needed(&mut self) {
        if self.hotkey_install_attempted {
            return;
        }
        self.hotkey_install_attempted = true;

        // Empty spec is the "explicitly disabled" sentinel (config `none`).
        if let Some(spec) = self.config.quick_terminal_hotkey.clone()
            && !spec.trim().is_empty()
        {
            match crate::macos_hotkey::GlobalHotKey::register(
                &spec,
                self.proxy.clone(),
                crate::macos_hotkey::HotkeyAction::QuickTerminal,
            ) {
                Some(hotkey) => self.quick_terminal_hotkey = Some(hotkey),
                None => log::warn!("failed to register quick-terminal-hotkey `{spec}`"),
            }
        }

        if let Some(spec) = self.config.sidebar_hotkey.clone()
            && !spec.trim().is_empty()
        {
            match crate::macos_hotkey::GlobalHotKey::register(
                &spec,
                self.proxy.clone(),
                crate::macos_hotkey::HotkeyAction::Sidebar,
            ) {
                Some(hotkey) => self.sidebar_hotkey = Some(hotkey),
                None => log::warn!("failed to register sidebar-hotkey `{spec}`"),
            }
        }
    }

    /// Toggle the quick terminal: reveal it (creating its window on first use)
    /// when hidden, slide it away when shown. A no-op before the GPU exists
    /// (i.e. before the first real window), which also means it can't be the
    /// app's only window.
    pub(super) fn toggle_quick_terminal(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_none() {
            return;
        }
        match self.quick_terminal.as_ref() {
            Some(qt) if qt.visible => self.start_quick_terminal_hide(),
            _ => self.start_quick_terminal_show(event_loop),
        }
    }

    /// The target monitor's origin and the panel's full-width x fractional-
    /// height footprint, all in physical pixels.
    fn quick_terminal_geometry(
        &self,
        event_loop: &ActiveEventLoop,
    ) -> Option<(i32, i32, u32, u32)> {
        let monitor = event_loop.primary_monitor().or_else(|| {
            self.focused_window()
                .and_then(|window| window.current_monitor())
        })?;
        let position = monitor.position();
        let size = monitor.size();
        let height = quick_terminal_height(size.height, self.config.quick_terminal_size);
        Some((position.x, position.y, size.width, height))
    }

    fn start_quick_terminal_show(&mut self, event_loop: &ActiveEventLoop) {
        let Some((origin_x, top_y, width, height)) = self.quick_terminal_geometry(event_loop)
        else {
            return;
        };
        if self.quick_terminal.is_none() {
            let Some(window_id) =
                self.create_quick_terminal(event_loop, origin_x, top_y, width, height)
            else {
                return;
            };
            self.quick_terminal = Some(QuickTerminalState {
                window_id,
                origin_x,
                top_y,
                width,
                height,
                visible: false,
                anim: None,
            });
        } else if let Some(qt) = self.quick_terminal.as_mut() {
            // Re-derive geometry each open: the active monitor (or its
            // resolution) may have changed since last time.
            qt.origin_x = origin_x;
            qt.top_y = top_y;
            qt.width = width;
            qt.height = height;
            if let Some(state) = self.windows.get(&qt.window_id) {
                let _ = state
                    .window
                    .request_inner_size(PhysicalSize::new(width, height));
            }
        }

        let Some(qt) = self.quick_terminal.as_mut() else {
            return;
        };
        let now = Instant::now();
        let from_reveal = qt.current_reveal(now);
        qt.visible = true;
        qt.anim = Some(QuickTerminalAnim::new(now, from_reveal, 1.0));
        let window_id = qt.window_id;
        let current_top =
            top_y + quick_terminal_reveal_top_offset(height as f32, from_reveal).round() as i32;
        if let Some(state) = self.windows.get(&window_id) {
            state
                .window
                .set_outer_position(PhysicalPosition::new(origin_x, current_top));
            state.window.set_visible(true);
            state.window.focus_window();
            state.window.request_redraw();
        }
        self.focused = Some(window_id);
    }

    pub(super) fn start_quick_terminal_hide(&mut self) {
        let Some(qt) = self.quick_terminal.as_mut() else {
            return;
        };
        let already_hiding = !qt.visible && qt.anim.as_ref().is_some_and(|anim| anim.hides());
        if already_hiding {
            return;
        }
        let now = Instant::now();
        let from_reveal = qt.current_reveal(now);
        qt.visible = false;
        qt.anim = Some(QuickTerminalAnim::new(now, from_reveal, 0.0));
        let window_id = qt.window_id;
        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    /// Hide the quick terminal when it loses focus, if `quick-terminal-autohide`
    /// is enabled. Called from the window's `Focused(false)` event.
    pub(super) fn maybe_autohide_quick_terminal(&mut self) {
        if !self.config.quick_terminal_autohide {
            return;
        }
        if self.quick_terminal.as_ref().is_some_and(|qt| qt.visible) {
            self.start_quick_terminal_hide();
        }
    }

    /// Advance the slide, repositioning the window each frame. Reports the next
    /// wake instant while animating (folded into `about_to_wait`'s deadline),
    /// and `None` once the slide settles — hiding the window on a completed
    /// slide-out.
    pub(super) fn tick_quick_terminal(&mut self) -> Option<Instant> {
        let (window_id, origin_x, top_y, height, anim) = {
            let qt = self.quick_terminal.as_ref()?;
            let anim = *qt.anim.as_ref()?;
            (qt.window_id, qt.origin_x, qt.top_y, qt.height, anim)
        };
        let now = Instant::now();
        let reveal = anim.reveal_at(now);
        let top = top_y + quick_terminal_reveal_top_offset(height as f32, reveal).round() as i32;
        if let Some(state) = self.windows.get(&window_id) {
            state
                .window
                .set_outer_position(PhysicalPosition::new(origin_x, top));
            state.window.request_redraw();
        }
        if anim.done(now) {
            if let Some(qt) = self.quick_terminal.as_mut() {
                qt.anim = None;
            }
            if anim.hides()
                && let Some(state) = self.windows.get(&window_id)
            {
                state.window.set_visible(false);
            }
            return None;
        }
        Some(now + QUICK_TERMINAL_FRAME_INTERVAL)
    }

    /// Tear down the quick terminal outright (its shell exited). Unlike hide,
    /// this drops the window and io thread so a fresh one is spawned next open.
    ///
    /// No session-store reconcile is needed here: a quick-terminal pane is never
    /// sidebar-eligible, so `apply_session_delta` drops its `Upsert`/`Bell`
    /// before they reach the store (FR-14/AC-16b) — there is never a QT card to
    /// leave behind.
    pub(super) fn destroy_quick_terminal(&mut self) {
        let Some(qt) = self.quick_terminal.take() else {
            return;
        };
        if let Some(mut state) = self.windows.remove(&qt.window_id) {
            state.shutdown();
        }
        if self.focused == Some(qt.window_id) {
            self.focused = self.window_order.last().copied();
            if let Some(window_id) = self.focused
                && let Some(state) = self.windows.get(&window_id)
            {
                state.window.focus_window();
            }
        }
    }

    /// Build the quick-terminal window + its single pane, inserting it into
    /// `windows` (but deliberately not `window_order`). Assumes the GPU is
    /// already initialized (guaranteed by `toggle_quick_terminal`).
    fn create_quick_terminal(
        &mut self,
        event_loop: &ActiveEventLoop,
        origin_x: i32,
        top_y: i32,
        width: u32,
        height: u32,
    ) -> Option<WindowId> {
        let attrs = WindowAttributes::default()
            .with_title("Quick Terminal")
            .with_decorations(false)
            .with_inner_size(PhysicalSize::new(width, height))
            .with_position(PhysicalPosition::new(origin_x, top_y - height as i32))
            .with_transparent(self.config.background_opacity < 1.0);
        #[cfg(target_os = "macos")]
        let attrs = attrs.with_option_as_alt(macos_option_as_alt(self.config.macos_option_as_alt));
        let window = Arc::new(event_loop.create_window(attrs).ok()?);
        window.set_ime_allowed(true);
        crate::macos_blur::apply_background_blur(
            &window,
            self.config.background_blur_radius,
            self.config.background_opacity,
        );
        crate::macos_window::configure_quick_terminal_window(&window);

        let surface = {
            let gpu = self.gpu.as_ref()?;
            gpu.instance.create_surface(window.clone()).ok()?
        };
        let (surface_config, renderer) = {
            let gpu = self.gpu.as_mut()?;
            let caps = surface.get_capabilities(&gpu.adapter);
            let surface_format = preferred_surface_format(&caps.formats);
            let size = window.inner_size();
            let surface_config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: preferred_surface_alpha_mode(
                    &caps,
                    self.config.background_opacity < 1.0,
                ),
                view_formats: vec![],
            };
            surface.configure(&gpu.device, &surface_config);
            let mut renderer = Renderer::new(
                &gpu.device,
                &gpu.queue,
                surface_format,
                &mut gpu.font,
                self.padding,
            )
            .ok()?;
            renderer.set_background_opacity(self.config.background_opacity);
            renderer.set_background_image(&gpu.device, &gpu.queue, self.background_image.clone());
            renderer.resize(PixelSize {
                w: surface_config.width,
                h: surface_config.height,
            });
            (surface_config, renderer)
        };

        let window_id = window.id();
        let initial_pane = PaneId::new(1);
        let initial_rect = PaneRectApp::new(0, 0, surface_config.width, surface_config.height);
        let metrics = self.gpu.as_ref()?.font.metrics();
        let grid = grid_size_for_pane_rect(initial_rect, metrics, self.padding);
        let initial_surface = self
            .spawn_pane_surface(window_id, initial_pane, grid, initial_rect, None)
            .ok()?;
        let mut surfaces = HashMap::new();
        surfaces.insert(initial_pane, initial_surface);
        let group = self.allocate_group_id();
        self.windows.insert(
            window_id,
            WindowState {
                window,
                group,
                surface,
                surface_config,
                renderer,
                split_tree: SplitTree::leaf(initial_pane),
                zoomed: None,
                focused_pane: initial_pane,
                next_pane_id: 2,
                surfaces,
                last_mouse_pane: Some(initial_pane),
                last_mouse_point: None,
                active_split_drag: None,
                occluded: false,
                title: "Noa".to_string(),
                sidebar_scroll: 0,
                sidebar_button_hover: false,
                sidebar_menu: None,
                sidebar_drag: None,
                link_click_in_flight: false,
                last_grid: None,
                resize_overlay: None,
                bell_flash_until: None,
                native_overlays: Default::default(),
            },
        );
        self.relayout_and_resize_window(window_id);
        Some(window_id)
    }
}
