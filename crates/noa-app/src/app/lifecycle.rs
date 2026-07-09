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
        // Resolve which logical window (tab group) this tab joins before the
        // window is created — the macOS `tabbingIdentifier` is baked into the
        // window attributes and can't change afterward.
        let focused_group = self
            .focused
            .and_then(|id| self.windows.get(&id))
            .map(|state| state.group);
        let group = match spawn_group_choice(target, focused_group) {
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

        let mut first_font = if self.gpu.is_none() {
            Some(
                FontGrid::new(
                    font_pixel_size(self.runtime_font_size, monitor_scale_factor),
                    font_config_from_noa_config(&self.config.font),
                )
                .expect("failed to load a system monospace font"),
            )
        } else {
            None
        };
        let metrics = first_font
            .as_ref()
            .map(FontGrid::metrics)
            .or_else(|| self.gpu.as_ref().map(|gpu| gpu.font.metrics()))
            .expect("font must exist before creating a tab");
        let inner_size = initial_window_logical_size(
            metrics,
            initial_grid_size,
            monitor_scale_factor,
            self.padding,
        );

        let window_attrs = self.tab_window_attributes(inner_size, group);
        let window = Arc::new(
            event_loop
                .create_window(window_attrs)
                .expect("failed to create window"),
        );
        let window_scale_factor = window.scale_factor();
        if let Some(font) = first_font.as_mut()
            && (window_scale_factor - monitor_scale_factor).abs() > f64::EPSILON
        {
            *font = FontGrid::new(
                font_pixel_size(self.runtime_font_size, window_scale_factor),
                font_config_from_noa_config(&self.config.font),
            )
            .expect("failed to load a system monospace font");
            let inner_size = initial_window_logical_size(
                font.metrics(),
                initial_grid_size,
                window_scale_factor,
                self.padding,
            );
            let _ = window.request_inner_size(inner_size);
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

        let surface = if self.gpu.is_none() {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
            let surface = instance.create_surface(window.clone()).unwrap_or_else(|e| {
                gpu_init_fatal(
                    &mut self.session_persister,
                    "could not create the window surface",
                    e,
                )
            });
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::default(),
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                }))
                .unwrap_or_else(|e| {
                    gpu_init_fatal(
                        &mut self.session_persister,
                        "no compatible GPU adapter found",
                        e,
                    )
                });
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("noa-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    experimental_features: wgpu::ExperimentalFeatures::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::Off,
                }))
                .unwrap_or_else(|e| {
                    gpu_init_fatal(
                        &mut self.session_persister,
                        "could not open a GPU device",
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
            self.gpu = Some(GpuState {
                instance,
                adapter,
                device,
                queue,
                pipelines: noa_render::PipelineCache::default(),
                font_atlases: noa_render::GlyphAtlasCache::default(),
                font: first_font.expect("first tab must initialize the font"),
                sidebar_font: FontGrid::new(
                    sidebar_font_pixel_size(window_scale_factor),
                    font_config_from_noa_config(&self.config.font),
                )
                .unwrap_or_else(|e| {
                    gpu_init_fatal(
                        &mut self.session_persister,
                        "could not load the sidebar font",
                        e,
                    )
                }),
                sidebar_font_atlases: noa_render::GlyphAtlasCache::default(),
                theme: {
                    let theme = crate::theme::resolve_theme_with_overrides(
                        self.config.theme.as_deref(),
                        &self.theme_overrides(),
                    );
                    // Chrome (sidebar/overview) polarity follows the terminal
                    // theme: a light theme gets light chrome.
                    crate::chrome::select_palette(theme.is_light());
                    theme
                },
                preview_theme: None,
                chrome_textures: ChromeTextures::default(),
                palette_renderer: None,
                palette_card: None,
                palette_padding: noa_core::GridPadding::ZERO,
                palette_scrim: None,
            });
            surface
        } else {
            let gpu = self.gpu.as_ref().expect("gpu initialized");
            gpu.instance
                .create_surface(window.clone())
                .unwrap_or_else(|e| {
                    gpu_init_fatal(
                        &mut self.session_persister,
                        "could not create the window surface",
                        e,
                    )
                })
        };

        let (surface_config, renderer) = {
            let gpu = self.gpu.as_mut().expect("gpu initialized");
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

            let pipelines = gpu.pipelines.get(&gpu.device, surface_format);
            let font_atlases =
                gpu.font_atlases
                    .get(&gpu.device, &gpu.queue, surface_format, &gpu.font);
            let mut renderer = Renderer::with_pipelines(
                &gpu.device,
                &gpu.queue,
                &pipelines,
                &font_atlases,
                &mut gpu.font,
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

        // A translucent window leaves native titlebar/tab chrome compositing
        // against undefined pixels; back the strip with an opaque theme view.
        #[cfg(target_os = "macos")]
        {
            let bg = self.gpu.as_ref().expect("gpu initialized").theme.default_bg;
            crate::macos_window::set_window_background_color(
                &window,
                bg,
                self.config.background_opacity,
            );
            if needs_macos_titlebar_backdrop(self.config.background_opacity) {
                crate::macos_window::install_titlebar_backdrop(&window, bg);
            }
        }

        let window_id = window.id();
        let initial_pane = PaneId::new(1);
        let initial_rect = content_inset_bounds(
            PaneRectApp::new(0, 0, surface_config.width, surface_config.height),
            crate::macos_window::top_chrome_inset_px(&window).unwrap_or_else(|| {
                titlebar_top_inset_px(self.config.macos_titlebar_style, window.scale_factor())
            }),
            content_margin_px(self.config.macos_titlebar_style, window.scale_factor()),
        );
        let auto_approve_enabled = Arc::new(AtomicBool::new(
            self.config.auto_approve && self.window_sidebar_eligible(window_id),
        ));
        let initial_surface = self.spawn_pane_surface(
            window_id,
            initial_pane,
            initial_grid_size,
            initial_rect,
            inherited_cwd,
            auto_approve_enabled.clone(),
        )?;
        let mut surfaces = HashMap::new();
        surfaces.insert(initial_pane, initial_surface);

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
                next_pane_id: 2,
                surfaces,
                last_mouse_pane: Some(initial_pane),
                last_mouse_point: None,
                active_split_drag: None,
                occluded: false,
                title: "Noa".to_string(),
                auto_approve_enabled,
                sidebar_scroll: 0,
                sidebar_button_hover: false,
                sidebar_card_hover: None,
                sidebar_menu: None,
                sidebar_drag: None,
                link_click_in_flight: false,
                file_drop: FileDropState::default(),
                last_grid: None,
                resize_overlay: None,
                bell_flash_until: None,
                title_override: None,
                native_overlays: Default::default(),
            },
        );
        self.window_order.push(window_id);
        self.mark_overview_tile_dirty(OverviewTileId::new(window_id, initial_pane));
        // A window seeded with the sidebar visible (`sidebar-enabled`) must turn
        // the io-thread gate on so its panes start publishing card state.
        self.refresh_sidebar_visible_gate();
        self.focused = Some(window_id);
        window.focus_window();
        self.request_overview_redraw();
        self.persist_session();
        Ok(window_id)
    }

    fn tab_window_attributes(
        &self,
        inner_size: LogicalSize<f64>,
        group: WindowGroupId,
    ) -> WindowAttributes {
        let attrs = WindowAttributes::default()
            .with_title("Noa")
            .with_inner_size(inner_size)
            // A transparent window is required for `background-opacity` to
            // reveal anything behind it; the surface alpha mode and the
            // renderer's clear alpha carry the actual opacity.
            .with_transparent(self.config.background_opacity < 1.0);
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
        let id = WindowGroupId(self.next_group_id);
        self.next_group_id += 1;
        id
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
    /// Stale or remote paths (which usually don't resolve locally) fall back
    /// to `None`, i.e. the process's own cwd.
    pub(super) fn pane_cwd(&self, window_id: WindowId, pane_id: PaneId) -> Option<String> {
        let cwd = self
            .windows
            .get(&window_id)?
            .surfaces
            .get(&pane_id)?
            .terminal
            .lock()
            .cwd
            .clone()?;
        std::path::Path::new(&cwd).is_dir().then_some(cwd)
    }

    /// cwd of the currently focused pane, for a newly spawned tab to inherit.
    fn focused_pane_cwd(&self) -> Option<String> {
        let window_id = self.focused?;
        let pane_id = self.windows.get(&window_id)?.focused_pane;
        self.pane_cwd(window_id, pane_id)
    }

    pub(super) fn spawn_pane_surface(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
        grid_size: GridSize,
        rect: PaneRectApp,
        cwd: Option<String>,
        auto_approve_enabled: Arc<AtomicBool>,
    ) -> anyhow::Result<Surface> {
        let pty_config = PtyConfig {
            size: grid_size,
            cwd,
            ..Default::default()
        };
        let pty = Pty::spawn(pty_config)?;
        // Hand the foreground-process probe to the session-metadata worker
        // (running-process display) before the pty moves into the io thread.
        // Quick-terminal panes never get a sidebar card, so they need no probe.
        if self.window_sidebar_eligible(window_id)
            && let Some(worker) = self.branch_poll.as_ref()
            && let Some(probe) = pty.foreground_probe()
        {
            worker.register_process_probe(Self::session_card_id(window_id, pane_id), probe);
        }
        let mut terminal = Terminal::new(grid_size);
        if let Some(style) = self.initial_cursor_style {
            terminal.set_default_cursor_style(style);
        }
        // A read (query) request is only queued when reads aren't fully
        // denied; the finer allow-vs-ask decision is made by the app layer
        // when a request arrives.
        terminal.osc52_policy.allow_read =
            self.config.clipboard_read != noa_config::ClipboardAccess::Deny;
        terminal.title_report = self.config.title_report;
        terminal.set_scrollback_limit_bytes(self.config.scrollback_limit);
        if let Some(gpu) = self.gpu.as_ref() {
            // Deliberately `gpu.theme` directly, not the `active_theme()`
            // resolver: a live theme preview must never reach a `Terminal`'s
            // `TerminalColors` (AC-2, spec L2 "Terminal生成箇所には手を入れない").
            terminal.set_base_colors(
                gpu.theme.default_fg,
                gpu.theme.default_bg,
                gpu.theme.cursor,
                gpu.theme.palette,
            );
        }
        let terminal = Arc::new(Mutex::new(terminal));
        let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
        let (pty_input_tx, pty_input_rx) = crate::io_thread::input_channel();
        let (auto_approve_feedback_tx, auto_approve_feedback_rx) = crossbeam_channel::unbounded();
        let auto_approve_guards = Arc::new(Mutex::new(
            crate::auto_approve::AutoApproveInputGuards::default(),
        ));
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
            enabled: auto_approve_enabled.clone(),
            guards: auto_approve_guards.clone(),
        };
        let io_thread = crate::io_thread::spawn(
            pty,
            terminal.clone(),
            self.proxy.clone(),
            crate::io_thread::IoThreadTarget { window_id, pane_id },
            resize_rx,
            pty_input_rx,
            auto_approve_feedback_rx,
            overview_publish,
            sidebar_publish,
            auto_approve,
        );

        Ok(Surface {
            terminal,
            pty_input_tx,
            auto_approve_feedback_tx,
            resize_tx,
            io_thread: Some(io_thread),
            grid_size,
            mouse_selection: MouseSelectionState::default(),
            selection_anchor: None,
            last_mouse_cell: None,
            pressed_mouse_button: None,
            ime_state: input::ImeState::default(),
            auto_approve_guards,
            rect,
            hover_link: None,
            overview_snapshot,
            snapshot_recycle: noa_render::FrameSnapshotRecycle::default(),
        })
    }

    fn pane_has_running_program(&self, window_id: WindowId, pane_id: PaneId) -> bool {
        self.windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .is_some_and(surface_has_running_program)
    }

    fn tab_running_program_count(&self, window_id: WindowId) -> usize {
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
        // The Overview overlay lives inside its host window; closing the host
        // tears the overlay down with it (before `close_tab_outcome`, so the
        // last-window case quits instead of keeping a ghost overlay alive).
        if self.overview_host() == Some(window_id) {
            self.overview_window = None;
            self.overview_visible = false;
            self.overview_visible_gate.store(false, Ordering::Relaxed);
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

        if let Some(mut state) = self.windows.remove(&window_id) {
            state.shutdown();
        }
        self.window_order.retain(|id| *id != window_id);
        self.overview_tiles
            .retain(|tile_id, _| tile_id.window_id != window_id);
        // A prompt targeting the closed window would otherwise linger
        // forever: its window can no longer deliver keys (so not even
        // Escape reaches it) and the open-guard would block every future
        // cmd+f app-wide.
        if self
            .search_prompt
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.search_prompt = None;
        }
        // C4 (R-11, FM3): the window-bound palette leaks the same way — a
        // closed window can deliver no keys (not even Escape), so a palette
        // still targeting it would strand a dead-window reference and the
        // toggle would never rebuild a fresh session. `close_pane` needs no
        // twin clear: the palette is window-bound, so a pane-only close
        // leaves it valid, and a whole-tab close always routes here.
        if self
            .command_palette
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.command_palette = None;
        }
        // Same leak shape again for the "Set Tab Title" prompt.
        if self
            .tab_title_prompt
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.tab_title_prompt = None;
        }
        // Same leak shape as the palette: a theme-settings overlay bound to
        // the closed window would strand a dead-window reference. Drop the
        // preview along with it — nothing else can clear it once its owning
        // window is gone.
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
        // Same leak shape as the palette: a confirm dialog bound to the closed
        // window could deliver no keys (not even Escape), stranding a modal.
        if self
            .confirm_dialog
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            self.confirm_dialog = None;
        }

        match outcome {
            TabCloseOutcome::Stale => {}
            TabCloseOutcome::Quit => {
                self.focused = None;
                event_loop.exit();
            }
            TabCloseOutcome::Continue { focused } => {
                self.focused = focused;
                if let Some(window) = self.focused_window() {
                    window.focus_window();
                } else if self.overview_visible {
                    self.focus_overview_window();
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
        let should_close_tab = self
            .windows
            .get(&window_id)
            .is_some_and(|state| state.contains_pane(pane_id) && state.pane_count() <= 1);
        if should_close_tab {
            self.close_tab(event_loop, window_id);
            return;
        }

        let mut tab_should_close = false;
        let window =
            {
                let Some(state) = self.windows.get_mut(&window_id) else {
                    return;
                };
                if !state.contains_pane(pane_id) {
                    return;
                }

                if let Some(mut surface) = state.surfaces.remove(&pane_id) {
                    surface.shutdown();
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
        self.overview_tiles
            .remove(&OverviewTileId::new(window_id, pane_id));
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
            if let Some(window_id) = self.window_order.get(index - 1).copied() {
                self.focused = Some(window_id);
                if let Some(window) = self.focused_window() {
                    window.focus_window();
                }
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
        if self.window_order.is_empty() {
            return;
        }
        let Some(focused) = self.focused else {
            return;
        };
        let Some(current) = self.window_order.iter().position(|id| *id == focused) else {
            return;
        };
        let len = self.window_order.len() as isize;
        let next = (current as isize + direction).rem_euclid(len) as usize;
        self.focused = Some(self.window_order[next]);
        if let Some(window) = self.focused_window() {
            window.focus_window();
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn install_macos_menu_if_needed(&mut self) {
        if self.macos_menu.is_none() {
            self.macos_menu = Some(
                crate::macos_menu::MacosMenu::install(self.proxy.clone())
                    .expect("failed to install macOS app menu"),
            );
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
        let split_enabled = self
            .windows
            .contains_key(&window_id)
            .then(|| crate::macos_menu::SplitContextMenuEnabled {
                left: self.can_create_split_in_window(window_id, Direction::Left),
                right: self.can_create_split_in_window(window_id, Direction::Right),
                up: self.can_create_split_in_window(window_id, Direction::Up),
                down: self.can_create_split_in_window(window_id, Direction::Down),
            })
            .unwrap_or_default();
        if let Err(error) =
            menu.show_split_context_menu(window.as_ref(), None, auto_approve_enabled, split_enabled)
        {
            log::debug!("failed to show macOS split context menu: {error:#}");
        }
    }
}
