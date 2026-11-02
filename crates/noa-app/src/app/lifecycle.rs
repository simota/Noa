//! Tab, window, and pane lifecycle operations.

use super::*;

impl App {
    pub(super) fn spawn_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        target: SpawnTarget,
    ) -> anyhow::Result<WindowId> {
        self.spawn_tab_with_cwd(event_loop, target, None)
    }

    /// Spawn a tab, optionally forcing the initial pane's cwd. `cwd_override`
    /// is `None` for interactive New Tab/New Window (which inherit the focused
    /// shell's cwd) and `Some(cwd)` for session restore, where the initial
    /// pane must open in its saved directory rather than inheriting.
    pub(super) fn spawn_tab_with_cwd(
        &mut self,
        event_loop: &ActiveEventLoop,
        target: SpawnTarget,
        cwd_override: Option<Option<String>>,
    ) -> anyhow::Result<WindowId> {
        // Inherit the focused shell's cwd before `self.focused` is repointed
        // at the new tab below (unless restore forces a specific cwd).
        let inherited_cwd = match cwd_override {
            Some(cwd) => cwd,
            None => self.focused_pane_cwd(),
        };
        self.spawn_tab_with_initial_pane(event_loop, target, InitialPane::Local { inherited_cwd })
    }

    /// Spawn a local tab for a scripted `new window`/`new tab` into `group`
    /// (agent-automation REQ-003/006), the ids and pty overrides coming from
    /// `App::script_spawn`.
    pub(super) fn spawn_script_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        group: GroupChoice<WindowGroupId>,
        cwd: Option<String>,
    ) -> anyhow::Result<WindowId> {
        self.spawn_tab_with_initial_pane_in_group(
            event_loop,
            group,
            InitialPane::Local { inherited_cwd: cwd },
        )
    }

    pub(super) fn spawn_detached_remote_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        target: SpawnTarget,
        identity: crate::remote_attach::RemotePaneIdentity,
    ) -> anyhow::Result<WindowId> {
        self.spawn_tab_with_initial_pane(event_loop, target, InitialPane::DetachedRemote(identity))
    }

    pub(super) fn spawn_detached_remote_tab_in_group(
        &mut self,
        event_loop: &ActiveEventLoop,
        group: WindowGroupId,
        identity: crate::remote_attach::RemotePaneIdentity,
    ) -> anyhow::Result<WindowId> {
        self.spawn_tab_with_initial_pane_in_group(
            event_loop,
            GroupChoice::Existing(group),
            InitialPane::DetachedRemote(identity),
        )
    }

    fn spawn_tab_with_initial_pane(
        &mut self,
        event_loop: &ActiveEventLoop,
        target: SpawnTarget,
        initial_spec: InitialPane,
    ) -> anyhow::Result<WindowId> {
        // Resolve which logical window (tab group) this tab joins before the
        // window is created — the macOS `tabbingIdentifier` is baked into the
        // window attributes and can't change afterward.
        let focused_group = self
            .focused
            .and_then(|id| self.windows.get(&id))
            .map(|state| state.group);
        let group = spawn_group_choice(target, focused_group);
        self.spawn_tab_with_initial_pane_in_group(event_loop, group, initial_spec)
    }

    fn spawn_tab_with_initial_pane_in_group(
        &mut self,
        event_loop: &ActiveEventLoop,
        group: GroupChoice<WindowGroupId>,
        initial_spec: InitialPane,
    ) -> anyhow::Result<WindowId> {
        // Preserve the current tab as an insertion anchor before creating the
        // new window. Only a live tab in the requested logical group can
        // anchor insertion; every other path keeps the existing append
        // behavior.
        let tab_anchor = match group {
            GroupChoice::Existing(group) => self.focused.filter(|window_id| {
                self.window_order.contains(window_id)
                    && self
                        .windows
                        .get(window_id)
                        .is_some_and(|state| state.group == group)
            }),
            GroupChoice::Fresh => None,
        };
        let tab_insert_at = tab_insert_index(&self.window_order, tab_anchor);
        let remote_card_identity = match &initial_spec {
            InitialPane::Local { .. } | InitialPane::Placeholder => None,
            InitialPane::DetachedRemote(identity) => Some(identity.clone()),
        };
        let group = match group {
            GroupChoice::Existing(group) => group,
            GroupChoice::Fresh => {
                let group = self.allocate_group_id();
                // A fresh logical window starts with the configured sidebar
                // default; a tab joining an existing group inherits that
                // group's current state by construction.
                if self.config.sidebar_enabled {
                    self.sidebar_visible_groups.insert(group);
                }
                group
            }
        };
        let initial_grid_size = GridSize::new(self.config.cols, self.config.rows);
        let monitor_scale_factor = event_loop
            .primary_monitor()
            .map(|monitor| monitor.scale_factor())
            .unwrap_or(1.0);

        let first_window = self.gpu.is_none();
        // Both font discoveries (terminal, primary-first + sidebar) have been
        // running on workers since the top of `crate::run` — before the event
        // loop was even built (startup W1). The first window consumes them:
        // only the primary face's metrics are needed to size the window
        // (available ~10 ms into the ~60 ms discovery); the full stacks are
        // joined later, right before the renderer needs glyphs.
        let mut font_tasks = if first_window {
            Some(
                self.startup_tasks
                    .take()
                    .expect("startup font tasks are consumed exactly once, by the first window"),
            )
        } else {
            None
        };
        let metrics = match font_tasks.as_ref().map(|tasks| {
            tasks.terminal_metrics(font_pixel_size(
                self.runtime_font_size,
                monitor_scale_factor,
            ))
        }) {
            Some(Ok(metrics)) => metrics,
            // An error means the worker failed before resolving a primary
            // face (no usable font / parse failure): join it to surface the
            // cause with the same fatal message the old inline load used.
            Some(Err(())) => {
                let (terminal_stack, _, _) =
                    font_tasks.take().expect("just matched Some").into_handles();
                let err = match terminal_stack.join() {
                    Ok(Err(e)) => format!("{e:?}"),
                    Ok(Ok(_)) => "font worker dropped its metrics channel".to_string(),
                    Err(_) => "font worker panicked".to_string(),
                };
                panic!("failed to load a system monospace font: {err}")
            }
            None => self
                .gpu
                .as_ref()
                .map(|gpu| gpu.fonts.primary().metrics())
                .expect("font must exist before creating a tab"),
        };
        let inner_size = initial_window_logical_size(
            metrics,
            initial_grid_size,
            monitor_scale_factor,
            self.padding,
        );

        // First window: created hidden, painted with the full startup
        // background (theme clear + background image when configured), and
        // only then shown (`early_show`) — the same pre-render-then-show
        // discipline as the quick terminal (RC1/RC2), so the window is on
        // screen long before fonts/renderer are ready and never flashes
        // unpainted/system-default content.
        let early_show = first_window;
        // Off macOS a tab joining a live group shares that group's OS window
        // and surface instead of opening a window of its own (see
        // `linux_tabs`).
        let shared_host = self.shared_tab_host(group);
        let joins_shared_window = shared_host.is_some();
        let window = match &shared_host {
            Some((window, _, _)) => window.clone(),
            None => {
                let window_attrs = self
                    .tab_window_attributes(inner_size, group)
                    .with_visible(!early_show);
                let window = Arc::new(
                    event_loop
                        .create_window(window_attrs)
                        .expect("failed to create window"),
                );
                crate::startup_trace::mark("window-created");
                // Seed the system-appearance snapshot from the first real
                // window we get a chance to ask (only meaningfully read once,
                // at startup — a `theme = light:...,dark:...` pair's initial
                // pick depends on it; live changes arrive via
                // `WindowEvent::ThemeChanged` afterward).
                if let Some(theme) = window.theme() {
                    self.system_appearance = theme;
                }
                window.set_ime_allowed(true);
                crate::macos_blur::apply_background_blur(
                    &window,
                    self.config.background_blur_radius,
                    self.config.background_opacity,
                );
                update_ime_cursor_area(
                    &window,
                    metrics,
                    0,
                    0,
                    PaneRectApp::new(0, 0, 0, 0),
                    self.padding,
                );
                window
            }
        };
        let window_scale_factor = window.scale_factor();

        let (surface, early_surface_config) = if let Some((_, surface, config)) = shared_host {
            // The shared surface is already configured; the config rides the
            // same no-reconfigure path as the first window's.
            (surface, Some(config))
        } else if let Some(gpu) = &self.gpu {
            let surface = gpu
                .instance
                .create_surface(window.clone())
                .unwrap_or_else(|e| {
                    gpu_init_fatal(
                        &mut self.session_persister,
                        "could not create the window surface",
                        e,
                    )
                });
            (Arc::new(surface), None)
        } else {
            // The GPU foundation (instance/adapter/device) has been warming on
            // a startup worker since the top of `crate::run`; by now it is
            // ready and the join is instant.
            let (terminal_stack_handle, sidebar_handle, gpu_handle) = font_tasks
                .take()
                .expect("first window consumes the startup font tasks")
                .into_handles();
            let PrewarmedGpu {
                instance,
                adapter,
                device,
                queue,
            } = match gpu_handle.join() {
                Ok(Ok(gpu)) => gpu,
                Ok(Err(msg)) => gpu_init_fatal(
                    &mut self.session_persister,
                    "could not initialize the GPU",
                    msg,
                ),
                Err(_) => gpu_init_fatal(
                    &mut self.session_persister,
                    "could not initialize the GPU",
                    "prewarm worker panicked",
                ),
            };
            let surface = instance.create_surface(window.clone()).unwrap_or_else(|e| {
                gpu_init_fatal(
                    &mut self.session_persister,
                    "could not create the window surface",
                    e,
                )
            });
            // Validation errors and device loss must not abort inside the
            // macOS winit delegate (non-unwinding); log them instead. Device
            // loss then surfaces as SurfaceError::Lost on the next frame and
            // goes through the reconfigure path in `redraw`.
            device.set_device_lost_callback(|reason, message| {
                log::error!("wgpu device lost ({reason:?}): {message}");
            });
            device.on_uncaptured_error(Arc::new(|err| {
                log::error!("wgpu uncaptured error: {err}");
            }));

            // Theme resolution needs only the config (~0.4 ms of work), so it
            // happens before the fonts join — the early solid frame below is
            // painted in the exact theme background color.
            let theme = crate::theme::resolve_theme_with_overrides(
                effective_theme_name(&self.config, self.system_appearance).as_deref(),
                &self.theme_overrides(),
            );
            // Chrome (sidebar/overview) polarity follows the terminal
            // theme: a light theme gets light chrome.
            // `glassmorphism` at any on-level resolves `background-opacity` /
            // `background-blur-radius` to that level's recommended glass pair
            // (`noa_config::apply_glassmorphism_defaults`), so the window
            // this palette draws into is always translucent and blurred —
            // there is no "frosted chrome over an opaque window" case left
            // to warn about here.
            crate::chrome::select_palette(theme.is_light(), self.config.glassmorphism);

            let caps = surface.get_capabilities(&adapter);
            // The adapter is picked before any window exists (prewarm), so on
            // multi-GPU / mixed Vulkan+GL Linux systems it may be unable to
            // present to this surface; `preferred_surface_format` would then
            // index an empty format list.
            if caps.formats.is_empty() {
                gpu_init_fatal(
                    &mut self.session_persister,
                    "the GPU adapter cannot present to this window",
                    adapter.get_info().name,
                );
            }
            let alpha_blending = alpha_blending_mode(&self.config.font);

            // If the window landed on a scale factor the monitor probe didn't
            // predict, the probe-sized surface would be the wrong physical
            // size. Settle the scale-corrected size *before* the first
            // `configure`/present so only one CAMetalLayer drawable generation
            // is ever allocated: drawables are allocated lazily by the first
            // `get_current_texture` (the present below), never by `configure`
            // alone, so a second `request_inner_size`-driven generation would
            // otherwise stay resident alongside the correct one. Deriving the
            // corrected size needs the real window-scale metrics, so on this
            // path the terminal font stack is joined up front; the common
            // (matched-scale) path still presents the startup frame before the
            // join to keep first paint early.
            let font_px = font_pixel_size(self.runtime_font_size, window_scale_factor);
            let mut terminal_stack_handle = Some(terminal_stack_handle);
            let mut prebuilt_font: Option<FontGrid> = None;
            let mut configure_size = None;
            if (window_scale_factor - monitor_scale_factor).abs() > f64::EPSILON {
                let font = build_terminal_font(
                    terminal_stack_handle.take().expect("handle present"),
                    font_px,
                    font_config_from_noa_config(&self.config.font),
                );
                let corrected = initial_window_logical_size(
                    font.metrics(),
                    initial_grid_size,
                    window_scale_factor,
                    self.padding,
                );
                // macOS applies this synchronously and returns the new size, so
                // the surface is configured at the corrected size on its first
                // (and only) `configure`. A platform that applies it
                // asynchronously returns `None`; the fall-through to
                // `window.inner_size()` then keeps the pre-correction size and
                // the later `Resized` reconfigures as before.
                configure_size = window.request_inner_size(corrected);
                prebuilt_font = Some(font);
            }

            let surface_config = build_surface_config(
                &caps,
                alpha_blending,
                configure_size.unwrap_or_else(|| window.inner_size()),
                self.config.background_opacity < 1.0,
            );
            surface.configure(&device, &surface_config);

            // W1 pre-render-then-show: put the native chrome backdrop in
            // place, paint the startup background (theme clear + background
            // image when configured) into the still-hidden window, and only
            // then order it front — the quick-terminal RC1/RC2 discipline, so
            // the window is on screen (~90 ms) while font discovery and
            // renderer bring-up are still running, and never flashes
            // unpainted/system-default content.
            if early_show {
                // A translucent window leaves native titlebar/tab chrome
                // compositing against undefined pixels; back the strip with an
                // opaque theme view — before the window is ever on screen.
                #[cfg(target_os = "macos")]
                {
                    crate::macos_window::set_window_background_color(
                        &window,
                        theme.default_bg,
                        self.config.background_opacity,
                    );
                    if needs_macos_titlebar_backdrop(
                        self.config.macos_titlebar_style,
                        self.config.background_opacity,
                        self.background_image.has_visible_image(),
                        self.config.glassmorphism.is_on(),
                    ) {
                        crate::macos_window::install_titlebar_backdrop(
                            &window,
                            theme.default_bg,
                            self.config.glassmorphism.is_on(),
                        );
                    }
                }
                match surface.get_current_texture() {
                    Ok(frame) => {
                        let view = frame
                            .texture
                            .create_view(&wgpu::TextureViewDescriptor::default());
                        noa_render::paint_startup_frame(
                            &device,
                            &queue,
                            &view,
                            surface_config.format,
                            PixelSize {
                                w: surface_config.width,
                                h: surface_config.height,
                            },
                            &theme,
                            self.config.background_opacity,
                            self.background_image.current_image(),
                        );
                        frame.present();
                        crate::startup_trace::mark("bg-frame-presented");
                    }
                    // The window still shows the NSWindow background color
                    // set above (the same theme bg), so showing it unpainted
                    // is safe — just unexpected enough to log.
                    Err(e) => log::warn!("startup pre-paint skipped: {e}"),
                }
                window.set_visible(true);
                // Push the show to the screen now: the event loop won't turn
                // (and flush AppKit's implicit transaction) until the rest of
                // this spawn — font join, renderer, pty — finishes.
                crate::macos_window::commit_window_display(&window);
                crate::startup_trace::mark("window-shown");
            }

            // The window is on screen; now join the terminal font stack (the
            // worker has been running since the top of `crate::run`) and
            // finish grid construction at the window's actual scale. The
            // scale-mismatch path above already joined it to size the surface.
            let font = match prebuilt_font {
                Some(font) => font,
                None => build_terminal_font(
                    terminal_stack_handle.take().expect("handle present"),
                    font_px,
                    font_config_from_noa_config(&self.config.font),
                ),
            };

            self.gpu = Some(GpuState {
                instance,
                adapter,
                device,
                queue,
                pipelines: noa_render::PipelineCache::default(),
                font_atlases: noa_render::GlyphAtlasCache::default(),
                fonts: super::font_cache::FontGridMap::from_grid(
                    font,
                    font_config_from_noa_config(&self.config.font),
                ),
                sidebar_fonts: super::font_cache::FontGridMap::from_grid(
                    {
                        // Stack prefetched on the startup worker; a join failure
                        // or load error (worth re-reporting through the fatal
                        // path) falls back to a fresh inline load.
                        let px_size = sidebar_font_pixel_size(
                            self.config.sidebar_font_size,
                            window_scale_factor,
                        );
                        let prefetched = sidebar_handle.join().ok().and_then(Result::ok).and_then(
                            |(stack, font_cfg)| FontGrid::with_stack(stack, px_size, font_cfg).ok(),
                        );
                        match prefetched {
                            Some(font) => font,
                            None => FontGrid::new(
                                px_size,
                                font_config_from_noa_config(&self.config.font),
                            )
                            .unwrap_or_else(|e| {
                                gpu_init_fatal(
                                    &mut self.session_persister,
                                    "could not load the sidebar font",
                                    e,
                                )
                            }),
                        }
                    },
                    font_config_from_noa_config(&self.config.font),
                ),
                sidebar_font_atlases: noa_render::GlyphAtlasCache::default(),
                theme,
                preview_theme: None,
                chrome_textures: ChromeTextures::default(),
                palette_renderer: None,
                palette_card: None,
                idle_ember_card: None,
                idle_ember_texture: None,
                idle_ember_pixels: Vec::new(),
                idle_ember_pose: None,
                palette_padding: noa_core::GridPadding::ZERO,
                palette_font_px: 0.0,
                palette_scrim: None,
                palette_shadow_source: None,
            });
            (Arc::new(surface), Some(surface_config))
        };

        let (surface_config, renderer) = {
            let gpu = self.gpu.as_mut().expect("gpu initialized");
            let alpha_blending = alpha_blending_mode(&self.config.font);
            let surface_config = match early_surface_config {
                // First window: the surface was configured (and painted)
                // before the fonts joined — reconfiguring here would discard
                // the already-presented solid startup frame.
                // A tab joining a shared window: the surface belongs to the
                // front tab and is already configured.
                Some(config) => config,
                None => {
                    let caps = surface.get_capabilities(&gpu.adapter);
                    let config = build_surface_config(
                        &caps,
                        alpha_blending,
                        window.inner_size(),
                        self.config.background_opacity < 1.0,
                    );
                    surface.configure(&gpu.device, &config);
                    config
                }
            };
            let surface_format = surface_config.format;

            let pipelines = gpu.pipelines.get(&gpu.device, surface_format);
            // Recomputed here rather than reused from the pre-paint scope
            // above: this is the pixel size THIS window rasterizes at, and it
            // is what keys both its font grid and its glyph atlas set.
            let font_px = font_pixel_size(self.runtime_font_size, window.scale_factor());
            gpu.fonts.ensure(font_px);
            // The sidebar band is drawn per window at the window's own scale,
            // so its grid has to exist for this window too — otherwise the
            // draw path falls back to the primary size.
            gpu.sidebar_fonts.ensure(sidebar_font_pixel_size(
                self.config.sidebar_font_size,
                window.scale_factor(),
            ));
            let font_atlases = gpu.font_atlases.get(
                &gpu.device,
                &gpu.queue,
                surface_format,
                gpu.fonts.get(font_px),
            );
            let mut renderer = Renderer::with_pipelines(
                &gpu.device,
                &gpu.queue,
                &pipelines,
                &font_atlases,
                gpu.fonts.get_mut(font_px),
                self.padding,
            )
            .unwrap_or_else(|e| {
                gpu_init_fatal(
                    &mut self.session_persister,
                    "could not build the renderer",
                    e,
                )
            });
            renderer.set_background_opacity(self.config.background_opacity);
            renderer.set_alpha_blending(alpha_blending);
            renderer.set_background_image(
                &gpu.device,
                &gpu.queue,
                self.background_image.current_image(),
            );
            renderer.resize(PixelSize {
                w: surface_config.width,
                h: surface_config.height,
            });
            (surface_config, renderer)
        };
        crate::startup_trace::mark("renderer-ready");

        // A translucent window leaves native titlebar/tab chrome compositing
        // against undefined pixels; back the strip with an opaque theme view.
        // (The early-show path installed this before ordering the window
        // front; installing twice would stack backdrop views.)
        #[cfg(target_os = "macos")]
        if !early_show {
            let bg = self.gpu.as_ref().expect("gpu initialized").theme.default_bg;
            crate::macos_window::set_window_background_color(
                &window,
                bg,
                self.config.background_opacity,
            );
            if needs_macos_titlebar_backdrop(
                self.config.macos_titlebar_style,
                self.config.background_opacity,
                self.background_image.has_visible_image(),
                self.config.glassmorphism.is_on(),
            ) {
                crate::macos_window::install_titlebar_backdrop(
                    &window,
                    bg,
                    self.config.glassmorphism.is_on(),
                );
            }
        }

        let window_id = if joins_shared_window {
            self.mint_tab_window_id()
        } else {
            window.id()
        };
        let initial_pane = self.next_pane_id();
        // A joining tab means the shared window has at least two tabs, so
        // the tab bar is up; the relayout after activation settles the rest.
        #[cfg(not(target_os = "macos"))]
        let tab_bar_px = if joins_shared_window {
            let gpu = self.gpu.as_ref().expect("gpu initialized");
            sidebar::tab_bar_geometry(
                gpu.fonts
                    .get(font_pixel_size(self.runtime_font_size, window_scale_factor))
                    .metrics(),
            )
            .1
        } else {
            0
        };
        #[cfg(target_os = "macos")]
        let tab_bar_px = 0;
        let initial_rect = content_inset_bounds(
            PaneRectApp::new(0, 0, surface_config.width, surface_config.height),
            crate::macos_window::top_chrome_inset_px(&window).unwrap_or_else(|| {
                titlebar_top_inset_px(self.config.macos_titlebar_style, window.scale_factor())
            }) + tab_bar_px,
            content_margin_px(self.config.macos_titlebar_style, window.scale_factor()),
        );
        let auto_approve_enabled = Arc::new(AtomicBool::new(
            self.config.auto_approve && self.window_sidebar_eligible(window_id),
        ));
        // One redraw-floor clock per window (tab), shared by every pane's io
        // thread it spawns — see `RedrawFloor`. Seeded from this window's
        // actual monitor refresh rate; `on_scale_factor_changed`/window-move
        // handling keeps it current if the window migrates monitors.
        let redraw_floor = crate::io_thread::RedrawFloor::new(
            crate::io_thread::redraw_floor_from_refresh_millihertz(
                window
                    .current_monitor()
                    .and_then(|monitor| monitor.refresh_rate_millihertz()),
            ),
        );
        let initial_surface = match initial_spec {
            InitialPane::Local { inherited_cwd } => self.spawn_pane_surface(
                window_id,
                initial_pane,
                initial_grid_size,
                initial_rect,
                inherited_cwd,
                auto_approve_enabled.clone(),
                redraw_floor.clone(),
            )?,
            InitialPane::DetachedRemote(identity) => {
                self.detached_remote_surface(initial_grid_size, initial_rect, identity)
            }
            InitialPane::Placeholder => self.placeholder_surface(initial_grid_size, initial_rect),
        };
        let mut surfaces = HashMap::new();
        surfaces.insert(initial_pane, initial_surface);

        let font_px = font_pixel_size(self.runtime_font_size, window.scale_factor());
        self.windows.insert(
            window_id,
            WindowState {
                created_transparent: window_created_transparent(self.config.background_opacity),
                window: window.clone(),
                font_px,
                group,
                tab_id: self
                    .script_spawn
                    .as_ref()
                    .and_then(|spawn| spawn.tab_id)
                    .unwrap_or_else(crate::macos_applescript::alloc_tab_id),
                ax: Default::default(),
                surface,
                surface_config,
                renderer,
                split_tree: SplitTree::leaf(initial_pane),
                zoomed: None,
                focused_pane: initial_pane,
                surfaces,
                last_mouse_pane: Some(initial_pane),
                mouse_capture_pane: None,
                last_mouse_point: None,
                last_mouse_physical_position: None,
                active_split_drag: None,
                modifiers: ModifiersState::empty(),
                key_modifiers: input::KeyModifierState::default(),
                occluded: false,
                background_tab: false,
                title: "Noa".to_string(),
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
                vt_resize_pending: Vec::new(),
                resize_overlay: None,
                bell_flash_until: None,
                title_override: None,
                native_overlays: Default::default(),
                applied_window_bg: None,
                bg_refresh_last: None,
                reveal_fast_path_pending: false,
            },
        );
        if let Some(identity) = remote_card_identity.as_ref() {
            self.register_remote_session_card(window_id, initial_pane, identity);
        }
        let inserted_after_anchor = tab_anchor
            .and_then(|anchor_id| self.windows.get(&anchor_id))
            .is_some_and(|anchor| {
                joins_shared_window
                    || crate::macos_window::insert_tab_after(&anchor.window, &window)
            });
        if inserted_after_anchor {
            self.window_order.insert(tab_insert_at, window_id);
        } else {
            // AppKit automatically appends new native tabs. If explicit
            // insertion is unavailable or fails, mirror that order rather
            // than letting Noa's navigation/session order diverge from it.
            self.window_order.push(window_id);
        }
        self.mark_overview_tile_dirty(OverviewTileId::new(window_id, initial_pane));
        // A window seeded with the sidebar visible (`sidebar-enabled`) must turn
        // the io-thread gate on so its panes start publishing card state.
        self.refresh_sidebar_visible_gate();
        if joins_shared_window {
            // Bring the new tab to the front of the shared window; every
            // sibling relayouts because the tab bar may just have appeared.
            self.activate_tab(window_id);
            self.focused = Some(window_id);
            self.relayout_tab_siblings(window_id);
        } else {
            #[cfg(not(target_os = "macos"))]
            self.tab_hosts.insert(window.id(), window_id);
            self.focused = Some(window_id);
        }
        if self.script_spawn.is_some() {
            crate::macos_window::make_key_without_activating(&window);
        } else {
            window.focus_window();
        }
        self.request_overview_redraw();
        self.persist_session();
        Ok(window_id)
    }

    fn tab_window_attributes(
        &self,
        inner_size: LogicalSize<f64>,
        group: WindowGroupId,
    ) -> WindowAttributes {
        let attrs = with_desktop_app_id(WindowAttributes::default())
            .with_title("Noa")
            .with_inner_size(inner_size)
            // A transparent window is required for `background-opacity` to
            // reveal anything behind it; the surface alpha mode and the
            // renderer's clear alpha carry the actual opacity.
            .with_transparent(window_created_transparent(self.config.background_opacity));
        #[cfg(target_os = "macos")]
        {
            // Tabs in the same group share a `tabbingIdentifier`, so AppKit
            // tabs them into one window; a distinct group id yields a distinct
            // identifier and thus a separate native window.
            let attrs = attrs
                .with_option_as_alt(macos_option_as_alt(self.config.macos_option_as_alt))
                .with_tabbing_identifier(&self.tabbing_identifier(group));
            apply_macos_titlebar_style(attrs, self.config.macos_titlebar_style)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = group;
            attrs
        }
    }

    /// Mint the next unique [`WindowGroupId`]. Each `New Window` calls this so
    /// its tab group can never collide with an existing window's.
    pub(super) fn allocate_group_id(&mut self) -> WindowGroupId {
        // A scripted `new window` pre-allocated its group id in the AE
        // handler (agent-automation REQ-008); everything else draws a fresh
        // one from the same process-wide counter.
        WindowGroupId(
            self.script_spawn
                .as_mut()
                .and_then(|spawn| spawn.group_id.take())
                .unwrap_or_else(crate::macos_applescript::alloc_group_id),
        )
    }

    /// The macOS `tabbingIdentifier` for a logical window. Per-process
    /// (`std::process::id()`) so two noa instances never merge their tabs, and
    /// per-group so each window keeps a separate tab bar.
    #[cfg(target_os = "macos")]
    fn tabbing_identifier(&self, group: WindowGroupId) -> String {
        format!("noa.tabs.{}.{}", std::process::id(), group.0)
    }

    /// The working directory reported by a pane's shell over OSC 7, if it
    /// points at a directory that still exists locally. A new tab or split
    /// inherits it so it opens where the focused shell is (Ghostty parity).
    /// Stale paths fall back to `None`, i.e. the process's own cwd. A remote
    /// pane's cwd is never used, even when it happens to resolve locally:
    /// it is whatever the remote server sent, and a local shell must not
    /// start in a directory a remote peer chose.
    pub(super) fn pane_cwd(&self, window_id: WindowId, pane_id: PaneId) -> Option<String> {
        let surface = self.windows.get(&window_id)?.surfaces.get(&pane_id)?;
        if surface.is_remote() {
            return None;
        }
        let cwd = surface.terminal.lock().cwd.clone()?;
        std::path::Path::new(&cwd).is_dir().then_some(cwd)
    }

    /// cwd of the currently focused pane, for a newly spawned tab (or the
    /// scratch terminal popup, scratch-terminal R3) to inherit.
    pub(super) fn focused_pane_cwd(&self) -> Option<String> {
        let window_id = self.focused?;
        let pane_id = self.windows.get(&window_id)?.focused_pane;
        self.pane_cwd(window_id, pane_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_pane_surface(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
        grid_size: GridSize,
        rect: PaneRectApp,
        cwd: Option<String>,
        auto_approve_enabled: Arc<AtomicBool>,
        redraw_floor: crate::io_thread::RedrawFloor,
    ) -> anyhow::Result<Surface> {
        let script = self.script_spawn.as_ref();
        let pty_config = PtyConfig {
            size: grid_size,
            cwd: script.and_then(|spawn| spawn.cwd.clone()).or(cwd),
            // A scripted spawn's `command` runs instead of the shell
            // (agent-automation Amendment 1). Otherwise one-shot: only the
            // first spawned surface runs the CLI `-e` command (see
            // `App::initial_command`); later tabs/splits find the slot empty
            // and get the normal shell.
            command: match script.and_then(|spawn| spawn.argv.clone()) {
                Some(argv) => Some(argv),
                None => self.initial_command.lock().take(),
            },
            env: script.map(|spawn| spawn.env.clone()).unwrap_or_default(),
            ..Default::default()
        };
        // The first spawn normally consumes the child already booted in
        // parallel by `App::new` (see `App::prespawned_pty`) — the `-e`
        // command when one was given, the default shell otherwise.
        let pty = match self.take_prespawned_pty(&pty_config) {
            Some(pty) => pty,
            None => Pty::spawn(pty_config)?,
        };
        crate::startup_trace::mark("pty-spawned");
        let (child_pid, tty_name) = (pty.child_pid(), pty.tty_name());
        // Hand the foreground-process probe to the session-metadata worker
        // (running-process display) before the pty moves into the io thread.
        // Quick-terminal panes never get a sidebar card, so they need no probe.
        if self.window_sidebar_eligible(window_id)
            && let Some(worker) = self.branch_poll.as_ref()
            && let Some(probe) = pty.foreground_probe()
        {
            worker.register_process_probe(Self::session_card_id(window_id, pane_id), probe);
        }
        let mut terminal = self.new_terminal(grid_size);
        apply_clipboard_policy(&mut terminal, &self.config);
        // A pane split into a hidden window or background tab starts hidden.
        if self.window_hidden(window_id) {
            terminal.set_visible(false);
        }
        let kitty_animation_flag = terminal.kitty_animation_flag();
        let terminal = Arc::new(Mutex::new(terminal));
        let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
        let (pty_input_tx, pty_input_rx) =
            crate::io_thread::input_channel_with_budget(pty.writer().budget());
        let (auto_approve_feedback_tx, auto_approve_feedback_rx) = crossbeam_channel::unbounded();
        let auto_approve_guards = Arc::new(Mutex::new(
            crate::auto_approve::AutoApproveInputGuards::default(),
        ));
        // P2-1: the io thread reads the auto-approve enable flag through
        // this swappable holder rather than the bare `Arc<AtomicBool>`
        // itself, so a later cross-tab move (`App::move_pane_to_tab_at`) can
        // re-point it at the destination tab's flag — including future
        // toggles, since the holder is re-read on every batch rather than
        // the value being copied once at spawn time.
        let auto_approve_flag = Arc::new(Mutex::new(auto_approve_enabled.clone()));
        // P2-4: same swappable-holder shape as `auto_approve_flag` above, for
        // this pane's redraw-floor clock (see `RedrawFloorHandle`).
        let redraw_floor_handle: crate::io_thread::RedrawFloorHandle =
            Arc::new(Mutex::new(redraw_floor));
        let overview_snapshot = Arc::new(Mutex::new(None));
        let overview_publish = crate::io_thread::OverviewPublish {
            slot: overview_snapshot.clone(),
            visible: self.overview_visible_gate.clone(),
        };
        let sidebar_publish = crate::io_thread::SidebarPublish {
            visible: self.sidebar_visible_gate.clone(),
            preview_lines: self.sidebar_preview_lines_gate.clone(),
        };
        let auto_approve = crate::io_thread::AutoApprovePublish {
            enabled: auto_approve_flag.clone(),
            guards: auto_approve_guards.clone(),
        };
        let ipc_tap = self.ipc_output_tap(window_id, pane_id);
        let raw_attach_tap = self.register_ipc_attach_pane(
            window_id,
            pane_id,
            terminal.clone(),
            pty_input_tx.clone(),
        );
        // Main-thread writer clone taken before `pty` moves into the io thread,
        // so keyboard/paste input writes straight to the writer thread.
        let pty_writer = pty.writer();
        let input_echo_seq = Arc::new(AtomicU64::new(0));
        // Pane-dnd L2(e): the io thread's event-target window id lives behind
        // this shared cell rather than baked into its closure, so a later
        // cross-tab move can redirect it without a respawn. `io_window_id`
        // (below) is the main-thread's clone of the same cell.
        let io_window_id = Arc::new(AtomicU64::new(u64::from(window_id)));
        let io_thread = crate::io_thread::spawn(
            pty,
            terminal.clone(),
            self.proxy.clone(),
            crate::io_thread::IoThreadTarget {
                window_id: io_window_id.clone(),
                pane_id,
            },
            resize_rx,
            pty_input_rx,
            auto_approve_feedback_rx,
            input_echo_seq.clone(),
            overview_publish,
            sidebar_publish,
            auto_approve,
            redraw_floor_handle.clone(),
            ipc_tap,
            raw_attach_tap,
        );

        let mut surface = Surface::new(
            terminal,
            SurfaceTransport::Local(LocalSurfaceTransport {
                pty_input_tx,
                pty_writer,
                input_echo_seq,
                auto_approve_feedback_tx,
                resize_tx,
                io_thread: Some(io_thread),
                io_window_id,
                auto_approve_flag,
                redraw_floor: redraw_floor_handle,
            }),
            grid_size,
            rect,
            auto_approve_guards,
            overview_snapshot,
            kitty_animation_flag,
        );
        surface.child_pid = child_pid;
        surface.tty_name = tty_name;
        surface.wait_after_command = script.is_some_and(|spawn| spawn.wait_after_command);
        Ok(surface)
    }

    fn new_terminal(&self, grid_size: GridSize) -> Terminal {
        let mut terminal = Terminal::new(grid_size);
        if let Some(style) = self.initial_cursor_style {
            terminal.set_default_cursor_style(style);
        }
        terminal.title_report = self.config.title_report;
        apply_xt_checksum_policy(&mut terminal, &self.config);
        terminal.scrollback_compression = self.config.scrollback_compression;
        terminal.set_scrollback_limit_bytes(self.config.scrollback_limit);
        terminal
            .set_scrollback_limit_lines(self.config.scrollback_limit_lines.unwrap_or(usize::MAX));
        terminal.set_kitty_image_limit(self.config.image_storage_limit);
        if let Some(gpu) = self.gpu.as_ref() {
            // Deliberately `gpu.theme` directly, not the `active_theme()`
            // resolver: a live theme preview must never reach a `Terminal`'s
            // `TerminalColors` (AC-2, spec L2 "Terminal生成箇所には手を入れない").
            terminal.set_base_colors(
                gpu.theme.default_fg,
                gpu.theme.default_bg,
                gpu.theme.cursor,
                apply_palette_overrides(gpu.theme.palette, &self.config.palette),
            );
        }
        terminal
    }

    /// A pane with no process behind it, standing in for one about to move in
    /// (see [`InitialPane::Placeholder`]).
    fn placeholder_surface(&self, grid_size: GridSize, rect: PaneRectApp) -> Surface {
        let identity = crate::remote_attach::RemotePaneIdentity {
            endpoint: String::new(),
            pane_id: 0,
            cached_title: None,
        };
        self.detached_remote_surface(grid_size, rect, identity)
    }

    /// Move `pane` out of `source_window` into a tab of its own: in the same
    /// window group, or a new window (`new_window`) — Ghostty 1.4.0's
    /// dragging a split out to a new tab or window, driven from the Overview.
    /// The tab is created around a placeholder pane, and the pane takes its
    /// place through the cross-tab engine ([`App::move_pane_to_tab_at`],
    /// which carries the Pty and all per-pane state), so it is laid out once
    /// at the new tab's full size. The new tab takes focus, so the Overview
    /// closes first. Returns whether the pane moved.
    pub(super) fn detach_pane_to_new_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        source_window: WindowId,
        pane: PaneId,
        new_window: bool,
    ) -> bool {
        let Some(group) = self.windows.get(&source_window).map(|state| state.group) else {
            return false;
        };
        let group = if new_window {
            GroupChoice::Fresh
        } else {
            GroupChoice::Existing(group)
        };
        // The overlay lives in its host window, which the new tab is about to
        // cover or take focus from: close it cleanly rather than leave it
        // marked visible behind the new tab.
        if self.overview_visible {
            self.hide_tab_overview();
        }
        let new_tab = match self.spawn_tab_with_initial_pane_in_group(
            event_loop,
            group,
            InitialPane::Placeholder,
        ) {
            Ok(window_id) => window_id,
            Err(err) => {
                log::warn!("failed to open a tab for the detached pane: {err}");
                return false;
            }
        };
        let Some(placeholder) = self.windows.get(&new_tab).map(|state| state.focused_pane) else {
            return false;
        };
        let moved = self.move_pane_to_tab_at(
            event_loop,
            source_window,
            pane,
            new_tab,
            DestPlacement::Replace(placeholder),
        );
        if !moved {
            self.close_tab(event_loop, new_tab);
        }
        moved
    }

    pub(super) fn detached_remote_surface(
        &self,
        grid_size: GridSize,
        rect: PaneRectApp,
        identity: crate::remote_attach::RemotePaneIdentity,
    ) -> Surface {
        let mut terminal = self.new_terminal(grid_size);
        terminal.osc52_policy.allow_read = false;
        terminal.set_reply_writes_enabled(false);
        terminal.kitty_images.set_local_media_enabled(false);
        let mut stream = noa_vt::Stream::new();
        stream.feed(
            b"\x1b[2J\x1b[HRemote pane detached.\r\nUse Attach Remote to reconnect.",
            &mut terminal,
        );
        let kitty_animation_flag = terminal.kitty_animation_flag();
        let terminal = Arc::new(Mutex::new(terminal));
        let auto_approve_guards = Arc::new(Mutex::new(
            crate::auto_approve::AutoApproveInputGuards::default(),
        ));
        let overview_snapshot = Arc::new(Mutex::new(None));
        Surface::new(
            terminal,
            SurfaceTransport::Remote(RemoteSurfaceTransport {
                identity,
                state: Arc::new(Mutex::new(
                    crate::remote_attach::RemoteAttachState::Detached,
                )),
                connection: None,
                card_seq: 1,
            }),
            grid_size,
            rect,
            auto_approve_guards,
            overview_snapshot,
            kitty_animation_flag,
        )
    }

    pub(super) fn register_remote_session_card(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        identity: &crate::remote_attach::RemotePaneIdentity,
    ) {
        let name = identity
            .cached_title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| format!("Remote pane {}", identity.pane_id));
        self.session_store
            .apply(crate::session_store::SessionDelta::Upsert {
                id: Self::session_card_id(window_id, pane_id),
                seq: 1,
                name,
                cwd: format!("REMOTE {}", identity.endpoint),
                busy: false,
                updated_at: crate::localtime::wall_clock_now(),
                preview: Some(vec![vec![crate::session_store::PreviewSpan {
                    text: "Detached — Retry Attach to reconnect".to_string(),
                    fg: noa_core::Color::Default,
                }]]),
            });
    }

    /// Mirror remote transport redraws into the same SessionDelta path local
    /// PTY output uses. Connection workers emit a targeted redraw for every
    /// state transition and raw output batch, so the card cannot remain stuck
    /// on its initial detached placeholder.
    pub(super) fn refresh_remote_session_card(&mut self, window_id: WindowId, pane_id: PaneId) {
        let preview_lines = self.config.sidebar_preview_lines;
        let Some((identity, remote_state, terminal, seq)) = (|| {
            let window = self.windows.get_mut(&window_id)?;
            let surface = window.surfaces.get_mut(&pane_id)?;
            let SurfaceTransport::Remote(remote) = &mut surface.transport else {
                return None;
            };
            remote.card_seq = remote.card_seq.saturating_add(1);
            Some((
                remote.identity.clone(),
                remote.state.lock().clone(),
                Arc::clone(&surface.terminal),
                remote.card_seq,
            ))
        })() else {
            return;
        };

        let (busy, preview) =
            remote_session_card_content(&remote_state, &terminal.lock(), preview_lines);
        let name = identity
            .cached_title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| format!("Remote pane {}", identity.pane_id));
        self.apply_session_delta(crate::session_store::SessionDelta::Upsert {
            id: Self::session_card_id(window_id, pane_id),
            seq,
            name,
            cwd: format!("REMOTE {}", identity.endpoint),
            busy,
            updated_at: crate::localtime::wall_clock_now(),
            preview: Some(preview),
        });
    }

    pub(super) fn pane_has_running_program(&self, window_id: WindowId, pane_id: PaneId) -> bool {
        self.windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .is_some_and(surface_has_running_program)
    }

    pub(super) fn tab_running_program_count(&self, window_id: WindowId) -> usize {
        self.windows
            .get(&window_id)
            .map(|state| running_program_count(state.surfaces.values()))
            .unwrap_or(0)
    }

    fn group_running_program_count(&self, group: WindowGroupId) -> usize {
        self.window_order
            .iter()
            .filter_map(|window_id| self.windows.get(window_id))
            .filter(|state| state.group == group)
            .map(|state| running_program_count(state.surfaces.values()))
            .sum()
    }

    fn app_running_program_count(&self) -> usize {
        self.windows
            .values()
            .map(|state| running_program_count(state.surfaces.values()))
            .sum()
    }

    pub(super) fn request_close_tab(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId) {
        let count = self.tab_running_program_count(window_id);
        if count > 0 {
            // Only the front tab of a shared OS window (off macOS) draws the
            // dialog and receives its keys, so a background tab closed from
            // the tab bar must come forward first.
            if self
                .windows
                .get(&window_id)
                .is_some_and(|state| state.background_tab)
            {
                self.focus_tab(window_id);
            }
            self.open_confirm_dialog(
                window_id,
                close_confirm_message(CloseConfirmTarget::Session, count),
                ConfirmAction::CloseTab { window_id },
            );
            return;
        }
        self.close_tab(event_loop, window_id);
    }

    pub(super) fn close_tab(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId) {
        // Before the surfaces are torn down and their keys are gone: a closed
        // tab's record must not outlive the tab.
        self.discard_window_records(window_id);
        self.end_copy_mode_for_window(window_id);
        // The Overview overlay lives inside its host window; closing the host
        // tears the overlay down with it (before `close_tab_outcome`, so the
        // last-window case quits instead of keeping a ghost overlay alive).
        if self.overview_host() == Some(window_id) {
            // Through the regular hide, so the panes' mirror snapshots are
            // released too, not just the overlay.
            self.hide_tab_overview();
            self.overview_window = None;
        }
        // A non-host tab closing keeps the overlay alive but removes its tiles;
        // an in-flight overview drag referencing the vanished window must not
        // survive (a no-op when the host closed above, or when no drag exists —
        // including the `move_pane_to_tab_at` self-call after it took the drag).
        self.cancel_overview_pane_drag();
        // Off macOS, closing the front tab of a shared window brings its
        // neighbour (next, else previous) to the front first, so the focus
        // repoint below lands inside the same OS window.
        let siblings: Vec<WindowId> = self
            .tab_siblings(window_id)
            .into_iter()
            .filter(|id| *id != window_id)
            .collect();
        let os_window = self.windows.get(&window_id).map(|state| state.window.id());
        let was_front_tab = !cfg!(target_os = "macos")
            && os_window.is_some_and(|os| self.tab_hosts.get(&os) == Some(&window_id));
        if was_front_tab {
            let index = self
                .tab_siblings(window_id)
                .iter()
                .position(|id| *id == window_id)
                .unwrap_or(0);
            if let Some(successor) = siblings.get(index).or(siblings.last()).copied() {
                self.activate_tab(successor);
            }
        }
        let outcome = close_tab_outcome(
            &self.window_order,
            self.focused,
            window_id,
            self.overview_visible,
        );
        if outcome == TabCloseOutcome::Stale {
            return;
        }

        let closing_panes: Vec<_> = self
            .windows
            .get(&window_id)
            .map(|state| state.surfaces.keys().copied().collect())
            .unwrap_or_default();
        for pane_id in closing_panes {
            self.cleanup_ipc_attach_pane(window_id, pane_id);
        }
        if let Some(mut state) = self.windows.remove(&window_id) {
            state.shutdown();
        }
        self.window_order.retain(|id| *id != window_id);
        // Overview tiles are tab-unit (U1): drop the closed tab's tile state.
        self.overview_tiles.remove(&window_id);
        self.clear_window_bound_overlays(window_id);
        if let Some(os_window) = os_window
            && self.tab_hosts.get(&os_window) == Some(&window_id)
        {
            self.tab_hosts.remove(&os_window);
        }
        // The tab bar may just have gone away (one tab left).
        for sibling in siblings {
            self.relayout_and_resize_window(sibling);
        }

        match outcome {
            TabCloseOutcome::Stale => {}
            TabCloseOutcome::Quit => {
                self.focused = None;
                event_loop.exit();
            }
            TabCloseOutcome::Continue { focused } => {
                self.focused = focused;
                let target_exists = self.focused_window().is_some();
                match tab_close_focus_decision(cfg!(target_os = "macos"), focused, target_exists) {
                    TabCloseFocusDecision::Deferred(window_id) => {
                        // AppKit transfers key/firstResponder after native-tab
                        // teardown, so re-focus on the next event-loop turn.
                        let _ = self.proxy.send_event(UserEvent::RestoreFocus { window_id });
                    }
                    TabCloseFocusDecision::Immediate(window_id) => {
                        // Off macOS the target may sit behind another tab of
                        // its own OS window.
                        self.focus_tab(window_id);
                        if let Some(state) = self.windows.get_mut(&window_id) {
                            // Mirror the deferred path's title re-assert: clear
                            // the applied-title mirror and request a redraw so
                            // the promoted window re-resolves its title rather
                            // than relying on `tab_title_update`'s steady-state
                            // skip (tab-close title-freeze fix).
                            state.title.clear();
                            state.window.request_redraw();
                        }
                    }
                    TabCloseFocusDecision::NoTarget if self.overview_visible => {
                        self.focus_overview_window();
                    }
                    TabCloseFocusDecision::NoTarget => {}
                }
                self.request_overview_redraw();
            }
        }
        // GC choke point (FR-12): the tab's cards (and, via window-remove, a
        // whole group's) are gone from `windows` now, so drop them from the
        // store too. `close_group`/`close_pane_after_pty_exit` reach this
        // through their `close_tab` calls.
        self.reconcile_session_store();
        // Drop sidebar visibility for a group whose last tab just closed, so
        // the set only ever holds live logical windows.
        let live_groups: HashSet<WindowGroupId> =
            self.windows.values().map(|state| state.group).collect();
        self.sidebar_visible_groups
            .retain(|group| live_groups.contains(group));
        // A closed window may have been the only one showing a sidebar.
        self.refresh_sidebar_visible_gate();
        self.persist_session();
    }

    /// Clear every window-bound overlay/modal session pinned to `window_id`
    /// once that window is gone (or, for the scratch terminal popup,
    /// destroyed outright — scratch-terminal fix 2). Each of these would
    /// otherwise strand a dead-window reference: the window can deliver no
    /// more keys (not even Escape), so a session still targeting it would
    /// linger forever and block whatever open-guard it holds app-wide.
    /// Shared by [`Self::close_tab`] and [`Self::destroy_scratch_terminal`]
    /// so the two teardown paths can't drift apart.
    pub(super) fn clear_window_bound_overlays(&mut self, window_id: WindowId) {
        if self
            .idle_ember
            .as_ref()
            .is_some_and(|(id, _)| *id == window_id)
        {
            self.clear_idle_ember();
        }
        if self
            .search_prompt
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.search_prompt = None;
        }
        if self
            .command_palette
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.command_palette = None;
        }
        if self
            .send_selection_picker
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.send_selection_picker = None;
        }
        if self
            .tab_title_prompt
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.tab_title_prompt = None;
        }
        if self
            .sidebar_rename
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.sidebar_rename = None;
        }
        if self
            .theme_settings
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.theme_settings = None;
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.preview_theme = None;
            }
        }
        if self
            .process_monitor
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.close_process_monitor();
        }
        if self
            .confirm_dialog
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
            && let Some(session) = self.confirm_dialog.take()
        {
            self.cancel_confirm_action(session.action);
        }
        if self
            .modal_preedit
            .as_ref()
            .is_some_and(|preedit| preedit.window_id == window_id)
        {
            self.modal_preedit = None;
        }
        // The `Attach Remote` overlay is window-bound the same way; a session
        // still targeting the closed/destroyed window would strand a dead
        // reference (its retry path re-resolves panes under `window_id`).
        if self
            .remote_ui
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.remote_ui = None;
        }
    }

    /// Close the entire focused logical window: every tab in its AppKit tab
    /// group (`cmd+shift+w` / File → Close Window). Each tab is torn down via
    /// [`App::close_tab`], so all its per-tab cleanup (io-thread shutdown,
    /// modal/search/palette de-leak, focus repoint) runs, and closing the last
    /// remaining window's last tab still quits the app through
    /// [`TabCloseOutcome::Quit`]. A no-op when nothing is focused.
    pub(super) fn request_close_window(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window_id) = self.focused else {
            return;
        };
        self.request_close_window_of(event_loop, window_id);
    }

    /// [`App::request_close_window`] for the logical window holding
    /// `window_id`.
    pub(super) fn request_close_window_of(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
    ) {
        let Some(group) = self.windows.get(&window_id).map(|state| state.group) else {
            return;
        };
        let count = self.group_running_program_count(group);
        if count > 0 {
            self.open_confirm_dialog(
                window_id,
                close_confirm_message(CloseConfirmTarget::Window, count),
                ConfirmAction::CloseWindow { group },
            );
            return;
        }
        self.close_group(event_loop, group);
    }

    pub(super) fn close_group(&mut self, event_loop: &ActiveEventLoop, group: WindowGroupId) {
        // Snapshot the group's tabs first — `close_tab` mutates `window_order`
        // and `focused` on each call, so iterating it live would be unsound.
        let tabs = ids_in_group(
            &self.window_order,
            |id| self.windows.get(&id).map(|state| state.group),
            group,
        );
        for window_id in tabs {
            self.close_tab(event_loop, window_id);
        }
    }

    pub(super) fn request_close_focused_pane_or_tab(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
    ) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        if state.pane_count() <= 1 {
            self.request_close_tab(event_loop, window_id);
            return;
        }
        let pane_id = state.focused_pane;
        self.request_close_pane(event_loop, window_id, pane_id);
    }

    pub(super) fn close_pane_after_pty_exit(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        pane_id: PaneId,
    ) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        if !state.contains_pane(pane_id) {
            return;
        }
        if state.pane_count() <= 1 {
            self.close_tab(event_loop, window_id);
            return;
        }
        self.close_pane(event_loop, window_id, pane_id);
    }

    pub(super) fn request_close_pane(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        pane_id: PaneId,
    ) {
        if self.pane_has_running_program(window_id, pane_id) {
            self.open_confirm_dialog(
                window_id,
                close_confirm_message(CloseConfirmTarget::Pane, 1),
                ConfirmAction::ClosePane { window_id, pane_id },
            );
            return;
        }
        self.close_pane(event_loop, window_id, pane_id);
    }

    pub(super) fn close_pane(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        pane_id: PaneId,
    ) {
        self.end_copy_mode_for_pane(window_id, pane_id);
        // A closing pane may be an overview drag's source or target tile — the
        // tile set is about to change, so cancel any in-flight overview drag.
        self.cancel_overview_pane_drag();
        let should_close_tab = self
            .windows
            .get(&window_id)
            .is_some_and(|state| state.contains_pane(pane_id) && state.pane_count() <= 1);
        if should_close_tab {
            self.close_tab(event_loop, window_id);
            return;
        }

        self.cleanup_ipc_attach_pane(window_id, pane_id);

        let mut tab_should_close = false;
        let window =
            {
                let Some(state) = self.windows.get_mut(&window_id) else {
                    return;
                };
                if !state.contains_pane(pane_id) {
                    return;
                }

                let closed_key = state
                    .surfaces
                    .get(&pane_id)
                    .and_then(|surface| surface.scrollback_key.clone());
                if state.mouse_capture_pane == Some(pane_id) {
                    state.mouse_capture_pane = None;
                }
                if let Some(mut surface) = state.surfaces.remove(&pane_id) {
                    surface.shutdown();
                }
                if let Some(key) = closed_key
                    && let Some(persister) = self.scrollback_persister.as_ref()
                {
                    persister.discard(key);
                }
                if self.search_prompt.as_ref().is_some_and(|session| {
                    session.window_id == window_id && session.pane_id == pane_id
                }) {
                    // The prompt's target pane is gone; keeping the session
                    // would leave a modal prompt bound to a dead pane and
                    // block every future cmd+f behind the open-guard.
                    self.search_prompt = None;
                }
                let outcome =
                    split_tree::close_pane_with_zoom(&mut state.split_tree, pane_id, state.zoomed);
                state.zoomed = outcome.zoomed;
                if outcome.close_outcome.tab_should_close {
                    tab_should_close = true;
                } else {
                    state.focused_pane = outcome
                        .close_outcome
                        .next_focus
                        .filter(|pane| state.contains_pane(*pane))
                        .or_else(|| state.surfaces.keys().copied().next())
                        .unwrap_or(state.focused_pane);
                    state.last_mouse_pane = Some(state.focused_pane);
                }
                state.window.clone()
            };

        if tab_should_close {
            self.close_tab(event_loop, window_id);
            return;
        }
        // The surviving tab's tile must recomposite without the closed pane
        // (Overview U1); its tab-tile state persists and is re-rendered fresh.
        self.mark_all_overview_tiles_dirty();
        // GC choke point (FR-12): the closed pane's card is dropped from the
        // store. `close_pane_after_pty_exit` reaches this through `close_pane`.
        self.reconcile_session_store();
        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
        self.request_overview_redraw();
        self.persist_session();
    }

    pub(super) fn request_quit(&mut self, event_loop: &ActiveEventLoop) {
        // scratch-terminal R4-f: tear the popup down explicitly rather than
        // leaving it visible behind (or as host of) a quit confirmation
        // dialog. `App`'s `Drop` also sweeps every window in `self.windows`
        // (including the popup's) on actual process exit, so this only
        // matters for the confirm-quit path below, not orphaned ptys.
        self.destroy_scratch_terminal();
        if !self.config.confirm_quit {
            event_loop.exit();
            return;
        }
        let count = self.app_running_program_count();
        let Some(window_id) = self
            .focused
            .filter(|id| self.windows.contains_key(id))
            .or_else(|| self.window_order.last().copied())
            .or_else(|| self.windows.keys().copied().next())
        else {
            event_loop.exit();
            return;
        };
        self.open_confirm_dialog(
            window_id,
            close_confirm_message(CloseConfirmTarget::App, count),
            ConfirmAction::Quit,
        );
    }

    pub(super) fn select_tab(&mut self, index: usize) {
        if index == 0 {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(window) = self.focused_window() {
                window.select_tab_at_index(index - 1);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let Some(focused) = self.focused else {
                return;
            };
            if let Some(tab) = self.tab_siblings(focused).get(index - 1).copied() {
                self.focus_tab(tab);
            }
        }
    }

    pub(super) fn select_next_tab(&mut self) {
        #[cfg(target_os = "macos")]
        {
            if let Some(window) = self.focused_window() {
                window.select_next_tab();
            }
        }
        #[cfg(not(target_os = "macos"))]
        self.cycle_fallback_tab(1);
    }

    pub(super) fn select_previous_tab(&mut self) {
        #[cfg(target_os = "macos")]
        {
            if let Some(window) = self.focused_window() {
                window.select_previous_tab();
            }
        }
        #[cfg(not(target_os = "macos"))]
        self.cycle_fallback_tab(-1);
    }

    #[cfg(not(target_os = "macos"))]
    fn cycle_fallback_tab(&mut self, direction: isize) {
        let Some(focused) = self.focused else {
            return;
        };
        let siblings = self.tab_siblings(focused);
        let Some(current) = siblings.iter().position(|id| *id == focused) else {
            return;
        };
        let len = siblings.len() as isize;
        let next = (current as isize + direction).rem_euclid(len) as usize;
        self.focus_tab(siblings[next]);
    }

    #[cfg(target_os = "macos")]
    pub(super) fn install_macos_menu_if_needed(&mut self) {
        if self.macos_menu.is_none() {
            self.macos_menu = Some(
                crate::macos_menu::MacosMenu::install(
                    self.proxy.clone(),
                    self.config.quick_terminal_hotkey.as_deref(),
                    self.keybinds
                        .chord_for(crate::commands::AppCommand::ToggleScratchTerminal)
                        .as_deref(),
                    self.keybinds
                        .chord_for(crate::commands::AppCommand::ToggleSidebar)
                        .as_deref(),
                )
                .expect("failed to install macOS app menu"),
            );
            self.set_ember_visible(self.config.ember);
        }
        if let Some(window_id) = self.focused {
            self.sync_macos_auto_approve_menu_state(window_id);
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn show_macos_split_context_menu(&self, window_id: WindowId) {
        let Some(menu) = self.macos_menu.as_ref() else {
            return;
        };
        let Some(window) = self
            .windows
            .get(&window_id)
            .map(|state| state.window.clone())
        else {
            return;
        };
        let auto_approve_enabled = self
            .windows
            .get(&window_id)
            .is_some_and(|state| state.auto_approve_enabled.load(Ordering::Relaxed));
        let split_enabled = if self.windows.contains_key(&window_id) {
            crate::macos_menu::SplitContextMenuEnabled {
                left: self.can_create_split_in_window(window_id, Direction::Left),
                right: self.can_create_split_in_window(window_id, Direction::Right),
                up: self.can_create_split_in_window(window_id, Direction::Up),
                down: self.can_create_split_in_window(window_id, Direction::Down),
            }
        } else {
            Default::default()
        };
        let send_selection_enabled = self
            .windows
            .get(&window_id)
            .map(|state| state.focused_pane)
            .is_some_and(|pane| self.can_open_send_selection_picker_for_pane(window_id, pane));
        if let Err(error) = menu.show_split_context_menu(
            window.as_ref(),
            None,
            auto_approve_enabled,
            split_enabled,
            send_selection_enabled,
        ) {
            log::debug!("failed to show macOS split context menu: {error:#}");
        }
    }
}

/// Join the terminal font-discovery worker and build the primary [`FontGrid`]
/// at `px_size`. A join or parse failure is fatal — the terminal cannot render
/// without a monospace face — matching the old inline load.
fn build_terminal_font(
    handle: std::thread::JoinHandle<Result<noa_font::FontStack, noa_font::FontError>>,
    px_size: f32,
    font_cfg: noa_font::FontConfig,
) -> FontGrid {
    let stack = match handle.join() {
        Ok(Ok(stack)) => stack,
        Ok(Err(e)) => panic!("failed to load a system monospace font: {e:?}"),
        Err(_) => panic!("failed to load a system monospace font: worker panicked"),
    };
    let font = FontGrid::with_stack(stack, px_size, font_cfg)
        .expect("failed to load a system monospace font");
    crate::startup_trace::mark("font-ready");
    font
}

fn remote_status_preview(text: &str) -> Vec<crate::session_store::PreviewLine> {
    vec![vec![crate::session_store::PreviewSpan {
        text: text.to_string(),
        fg: noa_core::Color::Default,
    }]]
}

fn remote_session_card_content(
    state: &crate::remote_attach::RemoteAttachState,
    terminal: &Terminal,
    preview_lines: usize,
) -> (bool, Vec<crate::session_store::PreviewLine>) {
    match state {
        crate::remote_attach::RemoteAttachState::Connected => {
            let rows = crate::io_thread::sidebar::preview_rows(terminal, preview_lines);
            let preview = crate::io_thread::sidebar::preview_spans(rows);
            (
                true,
                if preview.is_empty() {
                    remote_status_preview("Connected to remote pane")
                } else {
                    preview
                },
            )
        }
        crate::remote_attach::RemoteAttachState::Reconnecting { attempt, .. } => (
            false,
            remote_status_preview(&format!(
                "Reconnecting {attempt}/{}…",
                crate::remote_attach::MAX_RECONNECT_ATTEMPTS
            )),
        ),
        crate::remote_attach::RemoteAttachState::Detached => (
            false,
            remote_status_preview("Detached — Retry Attach to reconnect"),
        ),
    }
}

enum InitialPane {
    Local {
        inherited_cwd: Option<String>,
    },
    DetachedRemote(crate::remote_attach::RemotePaneIdentity),
    /// A process-less stand-in, replaced within the same call by a pane moved
    /// in from another tab ([`App::detach_pane_to_new_tab`]) — never drawn.
    Placeholder,
}

/// The clipboard settings a terminal enforces itself. An OSC 52 read is only
/// queued when reads aren't fully denied, and a denied write is dropped
/// (OSC 52) or answered `EPERM` (OSC 5522); the finer allow-vs-ask decision
/// is made by the app layer when a request arrives.
pub(super) fn apply_clipboard_policy(terminal: &mut noa_grid::Terminal, config: &AppConfig) {
    terminal.osc52_policy.allow_read = config.clipboard_read != noa_config::ClipboardAccess::Deny;
    terminal.osc52_policy.allow_write = config.clipboard_write != noa_config::ClipboardAccess::Deny;
    terminal.kitty_clipboard_write_limit = config.clipboard_write_limit_bytes;
}

/// DECRQCRA/XTCHECKSUM config. Like Ghostty, this also resets the variant a
/// program chose with XTCHECKSUM to the configured one.
pub(super) fn apply_xt_checksum_policy(terminal: &mut noa_grid::Terminal, config: &AppConfig) {
    let ext = config.vt_xt_checksum_extension;
    terminal.xt_checksum_report = config.vt_xt_checksum_report;
    terminal.set_default_xt_checksum(noa_grid::XtChecksumFlags {
        positive: !ext.negate,
        no_attributes: !ext.attributes,
        no_trim: !ext.trim,
        undrawn: ext.undrawn,
        full: ext.full,
    });
}

#[cfg(test)]
mod remote_session_card_tests {
    use super::*;

    #[test]
    fn connected_remote_card_uses_terminal_output_and_reconnect_uses_status() {
        let mut terminal = Terminal::new(GridSize::new(20, 3));
        let mut stream = Stream::new();
        stream.feed(b"remote output", &mut terminal);

        let (busy, preview) = remote_session_card_content(
            &crate::remote_attach::RemoteAttachState::Connected,
            &terminal,
            3,
        );
        assert!(busy);
        assert_eq!(
            crate::session_store::preview_line_text(&preview[0]),
            "remote output"
        );

        let (busy, preview) = remote_session_card_content(
            &crate::remote_attach::RemoteAttachState::Reconnecting {
                attempt: 4,
                delay: Duration::from_secs(8),
            },
            &terminal,
            3,
        );
        assert!(!busy);
        assert_eq!(
            crate::session_store::preview_line_text(&preview[0]),
            "Reconnecting 4/10…"
        );
    }
}
