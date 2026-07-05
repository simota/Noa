//! Tab-overview subsystem — `App` methods for the exposé-style tab
//! grid: showing/hiding the overview window, thumbnail rendering,
//! chrome/label textures, selection, and search.

use super::*;

impl App {
    pub(super) fn toggle_tab_overview(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(next_visible) = tab_overview_visibility_after_dispatch(
            AppCommand::ToggleTabOverview,
            self.overview_visible,
        ) {
            if next_visible {
                self.show_tab_overview(event_loop);
            } else {
                self.hide_tab_overview();
            }
        }
    }

    pub(super) fn show_tab_overview(&mut self, event_loop: &ActiveEventLoop) {
        if self.overview_window.is_none() {
            let window = Arc::new(
                event_loop
                    .create_window(self.overview_window_attributes())
                    .expect("failed to create Session Overview window"),
            );
            window.set_ime_allowed(false);

            // The overview window only ever opens once a tab already exists
            // (it is reachable only via a keybind/menu/command dispatched to
            // a live tab), so GPU state is always initialized here.
            let gpu = self
                .gpu
                .as_ref()
                .expect("gpu initialized before overview window opens");
            let surface = gpu
                .instance
                .create_surface(window.clone())
                .expect("failed to create wgpu overview surface");
            let caps = surface.get_capabilities(&gpu.adapter);
            let surface_format = preferred_surface_format(&caps.formats);
            let size = window.inner_size();
            let surface_config = wgpu::SurfaceConfiguration {
                // Tile cards and chrome pills are composited through render
                // passes; no direct copy into the surface is required.
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                // The overview window stays opaque: it composites tab tiles,
                // not live terminal background, so transparency would only
                // bleed the desktop through the switcher.
                alpha_mode: preferred_surface_alpha_mode(&caps, false),
                view_formats: vec![],
            };
            surface.configure(&gpu.device, &surface_config);

            self.overview_window = Some(OverviewWindowState {
                window,
                occluded: false,
                last_cursor_point: None,
                surface,
                surface_config,
                thumbnails: None,
                label_renderer: None,
                chrome_card: None,
                selected: 0,
                hovered: None,
                zoomed: false,
                search_query: String::new(),
            });
        }

        self.overview_visible = true;
        self.overview_visible_gate.store(true, Ordering::Relaxed);
        self.seed_overview_snapshots();
        self.mark_all_overview_tiles_dirty();
        // Reopening the Overview always starts with an empty filter (REQ-OV-16)
        // so the focused-tab initial selection below sees the full tab set —
        // and with hover/zoom state cleared, since neither survives a reopen
        // meaningfully.
        if let Some(overview) = self.overview_window.as_mut() {
            overview.search_query.clear();
            overview.hovered = None;
            overview.zoomed = false;
        }
        // REQ-OV-14: the focused pane's tile if it's live, else the first.
        let source_tile_ids = self.overview_source_tile_ids();
        let live_tile_count = OVERVIEW_GRID_CAP.min(source_tile_ids.len());
        let focused_tile = self.focused.and_then(|window_id| {
            let state = self.windows.get(&window_id)?;
            Some(OverviewTileId::new(window_id, state.focused_pane))
        });
        let selected =
            overview_initial_selection(&source_tile_ids, live_tile_count, focused_tile.as_ref());
        if let Some(overview) = self.overview_window.as_mut() {
            overview.selected = selected;
            overview.window.set_visible(true);
            overview.window.focus_window();
            overview.window.request_redraw();
        }
    }

    /// One-time re-peek for each open pane's overview mirror on every
    /// `show_tab_overview` call (Fix B). Once `overview_visible_gate` is
    /// set, each pane's io thread publishes a fresh `FrameSnapshot::peek`
    /// opportunistically on its own next pty output — but the gate was
    /// clear the whole time the overview was hidden, so a tab that kept
    /// producing output while hidden published nothing during that window,
    /// and its slot holds whatever it last published before hiding (or
    /// `None` on first open). Re-peeking unconditionally here — rather than
    /// only when the slot is still `None` — is what makes reopening show
    /// current content instead of that stale frame; a tab that publishes
    /// on its own moments later just gets overwritten immediately anyway.
    /// Runs once per `show_tab_overview` call, not per frame, so
    /// `render_due_overview_tiles` itself still never locks a pane's
    /// `Terminal`.
    pub(super) fn seed_overview_snapshots(&self) {
        for state in self.windows.values() {
            for surface in state.surfaces.values() {
                let Some(snapshot) = try_peek_overview_snapshot(&surface.terminal) else {
                    continue;
                };
                *surface.overview_snapshot.lock() = Some(snapshot);
            }
        }
    }

    pub(super) fn hide_tab_overview(&mut self) {
        self.overview_visible = false;
        self.overview_visible_gate.store(false, Ordering::Relaxed);
        if let Some(overview) = self.overview_window.as_ref() {
            overview.window.set_visible(false);
        }
    }

    pub(super) fn focus_overview_window(&self) {
        if let Some(overview) = self.overview_window.as_ref() {
            overview.window.focus_window();
        }
    }

    pub(super) fn is_overview_window(&self, window_id: WindowId) -> bool {
        self.overview_window
            .as_ref()
            .is_some_and(|overview| overview.window.id() == window_id)
    }

    pub(super) fn request_overview_redraw(&self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        if self.overview_visible && !overview.occluded {
            overview.window.request_redraw();
        }
    }

    pub(super) fn overview_window_occluded(&self) -> bool {
        self.overview_window
            .as_ref()
            .is_none_or(|overview| overview.occluded)
    }

    pub(super) fn mark_overview_tile_dirty(&mut self, tile_id: OverviewTileId) {
        self.overview_tiles.entry(tile_id).or_default().dirty = true;
    }

    pub(super) fn mark_all_overview_tiles_dirty(&mut self) {
        for tile_id in self.overview_source_tile_ids() {
            self.mark_overview_tile_dirty(tile_id);
        }
    }

    pub(super) fn overview_redraw_decision_for_pane(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
    ) -> TargetedRedrawDecision {
        overview_redraw_decision(
            self.windows
                .get(&window_id)
                .map(|state| (state.contains_pane(pane_id), state.occluded)),
            self.overview_visible,
            self.overview_window_occluded(),
        )
    }

    /// Build the pure due/backlog-decision input from each live source
    /// window's current dirty/last-render tile state. Shared by
    /// `due_overview_tile_ids` (pre-frame selection) and `redraw_overview`
    /// (post-frame backlog check), which read it at different points in
    /// the frame.
    pub(super) fn overview_tile_candidates(
        &self,
        source_tile_ids: &[OverviewTileId],
    ) -> Vec<OverviewRenderCandidate<OverviewTileId>> {
        source_tile_ids
            .iter()
            .filter_map(|tile_id| {
                let state = self.windows.get(&tile_id.window_id)?;
                if !state.contains_pane(tile_id.pane_id) {
                    return None;
                }
                let tile = self
                    .overview_tiles
                    .get(tile_id)
                    .copied()
                    .unwrap_or_default();
                Some(OverviewRenderCandidate {
                    id: *tile_id,
                    dirty: tile.dirty,
                    last_render_at: tile.last_render_at,
                })
            })
            .collect()
    }

    pub(super) fn due_overview_tile_ids(
        &self,
        source_tile_ids: &[OverviewTileId],
        now: Instant,
    ) -> Vec<OverviewTileId> {
        let candidates = self.overview_tile_candidates(source_tile_ids);
        select_due_overview_tile_ids(
            &candidates,
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL,
            OVERVIEW_MAX_RENDER_TILES_PER_FRAME,
        )
    }

    /// (Re)build the shared scratch + per-tile thumbnail textures (REQ-NF-3)
    /// whenever the grid layout, overview surface size, or surface format has
    /// drifted from what they were built for. Cheap to call every frame: the
    /// common case (nothing changed) is a handful of field comparisons.
    pub(super) fn ensure_overview_thumbnails(&mut self, layout: &OverviewLayout) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };

        // Placeholder tiles (REQ-OV-10) are the same uniform size as live
        // tiles (`rect_at` computes one `tile_w`/`tile_h` for the whole
        // grid), so they share this single texture pool; live tiles occupy
        // indices `[0, tiles.len())`, placeholders `[tiles.len(), tile_count)`.
        let tile_count = layout.tiles.len() + layout.placeholders.len();
        if tile_count == 0 {
            overview.thumbnails = None;
            return;
        }
        let tile_size = PixelSize {
            w: layout.tiles[0].w.max(1),
            h: layout.tiles[0].h.max(1),
        };
        let scratch_size = PixelSize {
            w: overview.surface_config.width,
            h: overview.surface_config.height,
        };
        let format = overview.surface_config.format;

        let stale = overview.thumbnails.as_ref().is_none_or(|thumbnails| {
            thumbnails.format() != format
                || thumbnails.tile_size() != tile_size
                || thumbnails.tile_count() != tile_count
        });
        if stale {
            overview.thumbnails = Some(OverviewThumbnailResources::new(
                &gpu.device,
                format,
                scratch_size,
                tile_size,
                tile_count,
                OVERVIEW_TITLE_BAR_H,
                OVERVIEW_CARD_COLOR,
            ));
        }
    }

    /// Render each due tile's source pane into the shared scratch texture and
    /// blit it down into that pane's tile texture (REQ-OV-4 live mirror,
    /// REQ-NF-1 reuse the tab's own `Renderer`, REQ-NF-3 shared-scratch
    /// blit-downscale). `tile_index` is `source_tile_ids`' position, which
    /// is index-parallel with `layout.tiles` (see `overview_tile_target_at_point`).
    pub(super) fn render_due_overview_tiles(
        &mut self,
        due_tile_ids: &[OverviewTileId],
        source_tile_ids: &[OverviewTileId],
    ) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        let Some(thumbnails) = overview.thumbnails.as_mut() else {
            return;
        };

        for &tile_id in due_tile_ids {
            let Some(tile_index) = source_tile_ids.iter().position(|id| *id == tile_id) else {
                continue;
            };
            let Some(state) = self.windows.get_mut(&tile_id.window_id) else {
                continue;
            };
            let Some(surface) = state.surfaces.get(&tile_id.pane_id) else {
                continue;
            };
            let source_viewport = PixelSize {
                w: surface.rect.w.max(1),
                h: surface.rect.h.max(1),
            };
            // Read-only publish slot (Fix B, REQ-NF-6): the io thread
            // already holds `Terminal`'s lock on every pty feed and
            // opportunistically publishes a `FrameSnapshot::peek` there
            // (cursor already hidden by `peek`), so this render path never
            // locks a tab's `Terminal` itself. `None` only for a tab that
            // hasn't published since the overview opened;
            // `seed_overview_snapshots`'s one-time fallback covers that gap.
            let Some(snapshot) = surface.overview_snapshot.lock().clone() else {
                continue;
            };

            // Reuse this tab's own `Renderer` unmodified (REQ-NF-1): point it
            // at the source pane's real pixel size just long enough to draw
            // one frame into the Overview scratch texture, then restore its
            // real surface viewport so the tab's own next redraw is unaffected.
            let own_viewport = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            state.renderer.resize(source_viewport);
            state
                .renderer
                .rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
            state
                .renderer
                .sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
            if let Err(err) = thumbnails.render_existing_renderer_to_tile(
                &gpu.device,
                &gpu.queue,
                &mut state.renderer,
                source_viewport,
                tile_index,
            ) {
                log::warn!(
                    "overview tile render failed for {:?}/pane {}: {err:#}",
                    tile_id.window_id,
                    tile_id.pane_id.get()
                );
            }
            state.renderer.resize(own_viewport);
        }
    }

    /// Lazily (re)build the dedicated placeholder-title `Renderer` (REQ-OV-10).
    /// A single instance is reused across every placeholder tile and frame —
    /// this does not create a per-tab renderer, so it doesn't conflict with
    /// REQ-NF-1's "reuse the tab's own `Renderer`" rule for live mirrors.
    pub(super) fn ensure_overview_label_renderer(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        let format = overview.surface_config.format;
        let stale = overview
            .label_renderer
            .as_ref()
            .is_none_or(|renderer| renderer.target_format() != format);
        if stale {
            overview.label_renderer = Some(
                Renderer::new(&gpu.device, &gpu.queue, format, &mut gpu.font, self.padding)
                    .expect("failed to build the overview label renderer"),
            );
        }
    }

    pub(super) fn ensure_overview_chrome_card_pipeline(&mut self) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        let format = overview.surface_config.format;
        let stale = overview
            .chrome_card
            .as_ref()
            .is_none_or(|chrome| chrome.format != format);
        if stale {
            overview.chrome_card = Some(OverviewChromeCardPipeline {
                format,
                pipeline: CardPipeline::new(&gpu.device, format, wgpu::BlendState::ALPHA_BLENDING),
            });
        }
    }

    /// Draw the source pane label into the top title-bar band of every due
    /// live tile
    /// (REQ-OV-12). Runs after `render_due_overview_tiles`, whose mirror blit
    /// re-clears the band to the card color, so the label must be re-stamped
    /// for exactly the tiles that were re-rendered this frame. Live labels are
    /// routed through the same tested `overview_tile_labels` seam as
    /// placeholder labels (AC-OV-12).
    pub(super) fn render_due_overview_title_bands(
        &mut self,
        due_tile_ids: &[OverviewTileId],
        source_tile_ids: &[OverviewTileId],
        layout: &OverviewLayout,
    ) {
        let live_count = layout.tiles.len().min(source_tile_ids.len());
        let live_ids = &source_tile_ids[..live_count];
        let labels = overview_tile_labels(live_ids, |id| self.overview_tile_label(id));
        let query = self
            .overview_window
            .as_ref()
            .map_or(String::new(), |overview| overview.search_query.clone());

        // Each live tile band carries its `⌘n` switch badge (REQ-OV-15c, only
        // the 1..=9 the keymap reaches), its status-dot color, and the live
        // search query for match highlighting (REQ-OV-16).
        let jobs: Vec<(usize, String, Option<usize>, Option<noa_core::Rgb>)> = labels
            .iter()
            .enumerate()
            .filter(|(index, _)| due_tile_ids.contains(&live_ids[*index]))
            .map(|(index, label)| {
                (
                    index,
                    label.label.clone(),
                    (index < 9).then_some(index + 1),
                    self.overview_tile_dot_color(live_ids[index]),
                )
            })
            .collect();
        for (tile_index, title, badge, dot) in jobs {
            self.render_tile_title_band(tile_index, &title, badge, dot, &query);
        }
    }

    /// Fill every placeholder-row tile (REQ-OV-10) with the card color and its
    /// source label band. Placeholders have no live mirror, so the whole tile is
    /// cleared to the card face before the title band is stamped on top.
    pub(super) fn render_overview_placeholder_labels(
        &mut self,
        source_tile_ids: &[OverviewTileId],
        layout: &OverviewLayout,
    ) {
        if layout.placeholders.is_empty() {
            return;
        }
        let live_count = layout.tiles.len();
        let overflow_ids = overview_placeholder_source_ids(source_tile_ids, live_count);
        let labels = overview_tile_labels(overflow_ids, |id| self.overview_tile_label(id));
        let query = self
            .overview_window
            .as_ref()
            .map_or(String::new(), |overview| overview.search_query.clone());
        let home = std::env::var("HOME").ok();

        // A placeholder has no live mirror to identify it, so its single band
        // row carries the session's cwd (and branch) after the title — pulled
        // from the same session store the sidebar reads.
        let jobs: Vec<(usize, String, Option<noa_core::Rgb>)> = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let tile_id = overflow_ids[index];
                let card_id = Self::session_card_id(tile_id.window_id, tile_id.pane_id);
                let title = match self.session_store.get(&card_id) {
                    Some(card) if !card.cwd.is_empty() => {
                        let cwd = crate::sidebar::format_cwd(&card.cwd, home.as_deref(), 24);
                        match &card.branch {
                            Some(branch) => format!("{} — {cwd} ⎇ {branch}", label.label),
                            None => format!("{} — {cwd}", label.label),
                        }
                    }
                    _ => label.label.clone(),
                };
                (
                    live_count + index,
                    title,
                    self.overview_tile_dot_color(tile_id),
                )
            })
            .collect();
        for (tile_index, title, dot) in jobs {
            if let (Some(gpu), Some(overview)) = (self.gpu.as_ref(), self.overview_window.as_ref())
                && let Some(thumbnails) = overview.thumbnails.as_ref()
            {
                thumbnails.clear_tile(&gpu.device, &gpu.queue, tile_index);
            }
            self.render_tile_title_band(tile_index, &title, None, dot, &query);
        }
    }

    /// Render `title` into `tile_index`'s dedicated title-band texture via the
    /// shared label `Renderer`, then stamp it onto the top `OVERVIEW_TITLE_BAR_H`
    /// rows of the tile (REQ-OV-12). The band is cleared to a distinct
    /// title-bar color (`set_clear_color` after `rebuild_cells`) so it reads as
    /// a band separate from the card face. Shared by live and placeholder
    /// tiles. `badge` prepends the dim `⌘n` switch number, `dot` colors the
    /// label's `● ` needs-user prefix, and `query`'s first match inside the
    /// label is accent-highlighted (REQ-OV-15c/16, sidebar-parity dots).
    pub(super) fn render_tile_title_band(
        &mut self,
        tile_index: usize,
        title: &str,
        badge: Option<usize>,
        dot: Option<noa_core::Rgb>,
        query: &str,
    ) {
        self.ensure_overview_label_renderer();
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        let (Some(label_renderer), Some(thumbnails)) = (
            overview.label_renderer.as_mut(),
            overview.thumbnails.as_mut(),
        ) else {
            return;
        };
        let tile_w = thumbnails.tile_size().w;
        let bar_h = thumbnails.title_bar_h();
        if tile_w == 0 || bar_h == 0 {
            return;
        }
        let band_size = PixelSize {
            w: tile_w.max(1),
            h: bar_h.max(1),
        };
        let grid_size = grid_size_for_pane_rect(
            PaneRectApp::new(0, 0, band_size.w, band_size.h),
            gpu.font.metrics(),
            DEFAULT_GRID_PADDING,
        );
        let sanitized = sanitize_placeholder_label(title, grid_size.cols);
        // REQ-OV-13: the centered title plus a close glyph in the last column,
        // with inline SGR styling (badge / dot / search highlight).
        let text = title_bar_row_ansi(&sanitized, grid_size.cols, badge, dot, query);

        let mut term = Terminal::new(GridSize::new(grid_size.cols, 1));
        Stream::new().feed(text.as_bytes(), &mut term);
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;

        label_renderer.resize(band_size);
        label_renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
        // After `rebuild_cells` (which resets it from the snapshot bg) so the
        // band gets its distinct title-bar color, not the terminal default.
        label_renderer.set_clear_color(OVERVIEW_TITLE_BAR_COLOR);
        label_renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);

        let Some(view) = thumbnails.title_texture_view(tile_index) else {
            return;
        };
        label_renderer.draw(&gpu.device, &gpu.queue, &view);
        thumbnails.stamp_title_band(&gpu.device, &gpu.queue, tile_index);
    }

    /// Render the top "Search sessions" field (REQ-OV-16) into a fresh pill-sized
    /// texture and return it for compositing into the reserved top search band.
    /// Shows the live query, or the placeholder while it is empty. `None` when
    /// there is no usable search band (a window too short to reserve one).
    pub(super) fn render_overview_search_texture(&mut self) -> Option<OverviewChromeTexture> {
        let chrome = self.overview_chrome()?;
        let rect = overview_search_field_rect(chrome.search_band);
        if rect.w == 0 || rect.h == 0 {
            return None;
        }
        let query = self
            .overview_window
            .as_ref()
            .map_or(String::new(), |overview| overview.search_query.clone());
        self.ensure_overview_label_renderer();
        let gpu = self.gpu.as_mut()?;
        let overview = self.overview_window.as_mut()?;
        let label_renderer = overview.label_renderer.as_mut()?;

        let band_size = PixelSize {
            w: rect.w.max(1),
            h: rect.h.max(1),
        };
        let search_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("noa-overview-search-pill"),
            size: wgpu::Extent3d {
                width: band_size.w,
                height: band_size.h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: overview.surface_config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = search_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let grid_size = grid_size_for_pane_rect(
            PaneRectApp::new(0, 0, band_size.w, band_size.h),
            gpu.font.metrics(),
            DEFAULT_GRID_PADDING,
        );
        let text = overview_search_field_row(&query, grid_size.cols);
        let mut term = Terminal::new(GridSize::new(grid_size.cols, 1));
        Stream::new().feed(text.as_bytes(), &mut term);
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;

        label_renderer.resize(band_size);
        label_renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
        label_renderer.set_clear_color(OVERVIEW_CHROME_PILL_COLOR);
        label_renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        label_renderer.draw(&gpu.device, &gpu.queue, &view);

        Some(OverviewChromeTexture {
            texture: search_texture,
            rect,
        })
    }

    /// Render the bottom hint bar (REQ-OV-17) into a fresh pill-sized texture
    /// and return it for compositing onto the surface. `None` when there is no
    /// usable hint band (a window too short to reserve one). The `⌘1-N` range
    /// tracks the live tile count dynamically.
    pub(super) fn render_overview_hint_texture(
        &mut self,
        live_tile_count: usize,
    ) -> Option<OverviewChromeTexture> {
        let chrome = self.overview_chrome()?;
        let rect = overview_hint_bar_rect(chrome.hint_band);
        if rect.w == 0 || rect.h == 0 {
            return None;
        }
        self.ensure_overview_label_renderer();
        let gpu = self.gpu.as_mut()?;
        let overview = self.overview_window.as_mut()?;
        let label_renderer = overview.label_renderer.as_mut()?;

        let band_size = PixelSize {
            w: rect.w.max(1),
            h: rect.h.max(1),
        };
        let hint_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("noa-overview-hint-pill"),
            size: wgpu::Extent3d {
                width: band_size.w,
                height: band_size.h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: overview.surface_config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = hint_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let grid_size = grid_size_for_pane_rect(
            PaneRectApp::new(0, 0, band_size.w, band_size.h),
            gpu.font.metrics(),
            DEFAULT_GRID_PADDING,
        );
        let text = center_label(&overview_hint_bar_text(live_tile_count), grid_size.cols);
        let mut term = Terminal::new(GridSize::new(grid_size.cols, 1));
        Stream::new().feed(text.as_bytes(), &mut term);
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;

        label_renderer.resize(band_size);
        label_renderer.rebuild_cells(&snapshot, &mut gpu.font, &gpu.theme);
        label_renderer.set_clear_color(OVERVIEW_CHROME_PILL_COLOR);
        label_renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        label_renderer.draw(&gpu.device, &gpu.queue, &view);

        Some(OverviewChromeTexture {
            texture: hint_texture,
            rect,
        })
    }

    /// Composite every live-mirror and placeholder tile onto the overview
    /// surface as a rounded card (REQ-OV-12/14), then overlay the bottom hint
    /// bar (REQ-OV-17), and present. Empty grid cells stay the backdrop color.
    pub(super) fn present_overview_frame(&mut self, layout: &OverviewLayout) {
        // Render the hint band first (it borrows the label renderer / gpu
        // mutably); the returned texture is owned, so the borrows are released
        // before compositing.
        let live_count = layout.tiles.len();
        let search_texture = self.render_overview_search_texture();
        let hint_texture = self.render_overview_hint_texture(live_count);
        self.ensure_overview_chrome_card_pipeline();
        let (selected, hovered, zoomed) = self
            .overview_window
            .as_ref()
            .map_or((0, None, false), |overview| {
                (overview.selected, overview.hovered, overview.zoomed)
            });
        // The zoom overlay centers within the grid band; resolved before the
        // gpu/overview borrows below.
        let zoom_bounds = self.overview_chrome().map(|chrome| chrome.grid_bounds);

        // Which tiles carry a pending interaction request (FR-16), indexed by
        // placement position (live tiles then placeholders, the same order as
        // `tile_rects` below). Resolved before the gpu/overview borrows so the
        // ring pass needs no `self` access. Held steady while pending (not
        // blink-gated) so the ring is a stable marker.
        let attention_tiles: Vec<bool> = self
            .overview_source_tile_ids()
            .iter()
            .map(|id| {
                let card_id = Self::session_card_id(id.window_id, id.pane_id);
                self.session_store
                    .get(&card_id)
                    .is_some_and(|card| card.attention)
            })
            .collect();

        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };

        let frame = match overview.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                overview
                    .surface
                    .configure(&gpu.device, &overview.surface_config);
                overview.window.request_redraw();
                return;
            }
            Err(e) => {
                log::warn!("overview surface error: {e}");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let surface_size = PixelSize {
            w: overview.surface_config.width,
            h: overview.surface_config.height,
        };

        // Card composite (also clears the surface to the backdrop color).
        if let Some(thumbnails) = overview.thumbnails.as_ref() {
            let live_count = layout.tiles.len();
            let placeholders = layout
                .placeholders
                .iter()
                .enumerate()
                .map(|(index, rect)| (live_count + index, rect));
            let placements: Vec<CardTilePlacement> = layout
                .tiles
                .iter()
                .enumerate()
                .chain(placeholders)
                .map(|(tile_index, rect)| CardTilePlacement {
                    tile_index,
                    x: rect.x,
                    y: rect.y,
                    w: rect.w,
                    h: rect.h,
                    selected: tile_index == selected,
                })
                .collect();
            thumbnails.composite_cards(
                &gpu.device,
                &gpu.queue,
                &view,
                surface_size,
                &OVERVIEW_CARD_STYLE,
                &placements,
            );
        } else {
            // No tiles: still clear the surface to the backdrop color.
            clear_overview_surface(&gpu.device, &gpu.queue, &view, OVERVIEW_BG_COLOR);
        }

        // Overlay the search and hint pills with the same rounded-card shader
        // as tiles, but without clearing the already-composited frame.
        if let Some(chrome_card) = overview.chrome_card.as_ref() {
            let search_view = search_texture.as_ref().map(|chrome| {
                chrome
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default())
            });
            let hint_view = hint_texture.as_ref().map(|chrome| {
                chrome
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default())
            });
            let mut placements = Vec::new();
            if let (Some(chrome), Some(view)) = (search_texture.as_ref(), search_view.as_ref()) {
                placements.push(CardTexturePlacement {
                    texture_view: view,
                    x: chrome.rect.x,
                    y: chrome.rect.y,
                    w: chrome.rect.w,
                    h: chrome.rect.h,
                    selected: false,
                });
            }
            if let (Some(chrome), Some(view)) = (hint_texture.as_ref(), hint_view.as_ref()) {
                placements.push(CardTexturePlacement {
                    texture_view: view,
                    x: chrome.rect.x,
                    y: chrome.rect.y,
                    w: chrome.rect.w,
                    h: chrome.rect.h,
                    selected: false,
                });
            }
            chrome_card.pipeline.overlay_texture_cards(
                &gpu.device,
                &gpu.queue,
                &view,
                surface_size,
                &OVERVIEW_CHROME_CARD_STYLE,
                &placements,
            );
        }

        // Hover accent ring + Tab quick-look zoom, composited above the grid
        // (and above the chrome, which never overlaps the tile rects anyway).
        if let (Some(thumbnails), Some(chrome_card)) =
            (overview.thumbnails.as_ref(), overview.chrome_card.as_ref())
        {
            let tile_rects: Vec<PaneRectApp> = layout
                .tiles
                .iter()
                .chain(layout.placeholders.iter())
                .copied()
                .collect();

            // A thin accent border over the hovered tile — subtler than the
            // selection's thick ring + glow, so the two stay distinguishable.
            if let Some(hovered) = hovered
                && hovered != selected
                && !zoomed
                && let Some(rect) = tile_rects.get(hovered)
                && let Some(tile_view) = thumbnails.tile_texture_view(hovered)
            {
                let hover_style = CardStyle {
                    focus_width: 1.5,
                    focus_glow_width: 0.0,
                    ..OVERVIEW_CARD_STYLE
                };
                chrome_card.pipeline.overlay_texture_cards(
                    &gpu.device,
                    &gpu.queue,
                    &view,
                    surface_size,
                    &hover_style,
                    &[CardTexturePlacement {
                        texture_view: &tile_view,
                        x: rect.x,
                        y: rect.y,
                        w: rect.w,
                        h: rect.h,
                        selected: true,
                    }],
                );
            }

            // The zoomed selected tile: same card texture, enlarged and
            // centered within the grid band with the full selection ring.
            if zoomed
                && let Some(bounds) = zoom_bounds
                && let Some(rect) = tile_rects.get(selected)
                && let Some(tile_view) = thumbnails.tile_texture_view(selected)
            {
                let zoom = overview_zoom_rect(bounds, *rect);
                chrome_card.pipeline.overlay_texture_cards(
                    &gpu.device,
                    &gpu.queue,
                    &view,
                    surface_size,
                    &OVERVIEW_CARD_STYLE,
                    &[CardTexturePlacement {
                        texture_view: &tile_view,
                        x: zoom.x,
                        y: zoom.y,
                        w: zoom.w,
                        h: zoom.h,
                        selected: true,
                    }],
                );
            }

            // Persistent red attention ring over every tile with a pending
            // interaction request (FR-16), drawn last so it sits above the
            // selection/hover rings — a request must stay visible even on the
            // focused or hovered tile. The zoomed tile is skipped (its enlarged
            // rect already carries the selection ring).
            for (index, rect) in tile_rects.iter().enumerate() {
                if !attention_tiles.get(index).copied().unwrap_or(false) {
                    continue;
                }
                if zoomed && index == selected {
                    continue;
                }
                let Some(tile_view) = thumbnails.tile_texture_view(index) else {
                    continue;
                };
                chrome_card.pipeline.overlay_texture_cards(
                    &gpu.device,
                    &gpu.queue,
                    &view,
                    surface_size,
                    &OVERVIEW_ATTENTION_CARD_STYLE,
                    &[CardTexturePlacement {
                        texture_view: &tile_view,
                        x: rect.x,
                        y: rect.y,
                        w: rect.w,
                        h: rect.h,
                        selected: true,
                    }],
                );
            }
        }

        frame.present();
    }

    pub(super) fn finish_overview_tile_renders(
        &mut self,
        tile_ids: &[OverviewTileId],
        now: Instant,
    ) {
        for tile_id in tile_ids {
            let tile = self.overview_tiles.entry(*tile_id).or_default();
            tile.dirty = false;
            tile.last_render_at = Some(now);
        }
    }

    pub(super) fn overview_source_tile_ids(&self) -> Vec<OverviewTileId> {
        let ordered = overview_tile_source_order(
            &self.window_order,
            |id| self.windows.contains_key(&id),
            |id| self.overview_pane_ids_for_window(id),
            None,
        )
        .into_iter()
        .map(|(window_id, pane_id)| OverviewTileId::new(window_id, pane_id))
        .collect::<Vec<_>>();
        // REQ-OV-16: the "Search sessions" filter narrows the source set here, the
        // single seam every downstream consumer (redraw / hit-test / nav /
        // Cmd+N / title bars / placeholders) reads, so the whole Overview sees
        // one filtered order. An empty query is the identity (short-circuited
        // to skip cloning titles on the common path).
        let query = self
            .overview_window
            .as_ref()
            .map_or("", |overview| overview.search_query.as_str());
        if query.is_empty() {
            return ordered;
        }
        let titles: Vec<(OverviewTileId, String)> = ordered
            .iter()
            .map(|id| {
                let title = self.overview_tile_label(*id).unwrap_or_default();
                (*id, title)
            })
            .collect();
        overview_tab_filter(query, &titles)
    }

    pub(super) fn overview_pane_ids_for_window(&self, window_id: WindowId) -> Vec<PaneId> {
        let Some(state) = self.windows.get(&window_id) else {
            return Vec::new();
        };
        split_tree::compute_layout(&state.split_tree, PaneRectApp::new(0, 0, 1001, 1001))
            .into_iter()
            .filter_map(|(pane_id, _)| state.contains_pane(pane_id).then_some(pane_id))
            .collect()
    }

    pub(super) fn overview_tile_label(&self, tile_id: OverviewTileId) -> Option<String> {
        let state = self.windows.get(&tile_id.window_id)?;
        if !state.contains_pane(tile_id.pane_id) {
            return None;
        }
        // A pane that needs a look (attention request / unread bell, FR-16) or
        // is running a program is marked with a leading `●` — the band renderer
        // colors it by the same dot semantics as the sidebar (red / yellow /
        // blue). The attention mark blinks in phase with the sidebar (FR-A2)
        // via `overview_tile_dot_color`'s blink gating.
        let title = if self.overview_tile_dot_color(tile_id).is_some() {
            format!("● {}", state.title)
        } else {
            state.title.clone()
        };
        if state.pane_count() <= 1 {
            return Some(title);
        }
        let pane_number = self
            .overview_pane_ids_for_window(tile_id.window_id)
            .iter()
            .position(|pane_id| *pane_id == tile_id.pane_id)
            .map(|index| index + 1)
            .unwrap_or_else(|| tile_id.pane_id.get() as usize);
        Some(format!("{title} [pane {pane_number}]"))
    }

    /// The Overview window's search / grid / hint bands (REQ-OV-11/16/17).
    /// The grid is laid out inside `grid_bounds`, so P3's search-field draw
    /// won't reflow the tiles, and the hint bar draws into `hint_band`.
    pub(super) fn overview_chrome(&self) -> Option<OverviewChrome> {
        let overview = self.overview_window.as_ref()?;
        let bounds = pane_bounds_for_size(overview.window.inner_size());
        Some(overview_chrome_bands(bounds))
    }

    pub(super) fn overview_layout(
        &self,
        source_tile_ids: &[OverviewTileId],
    ) -> Option<OverviewLayout> {
        let chrome = self.overview_chrome()?;
        Some(compute_overview_grid(
            source_tile_ids.len(),
            chrome.grid_bounds,
            OVERVIEW_GRID_CAP,
            OVERVIEW_TILE_GUTTER,
            OVERVIEW_OUTER_MARGIN,
        ))
    }

    pub(super) fn redraw_overview(&mut self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        if !self.overview_visible || overview.occluded {
            return;
        }

        let source_tile_ids = self.overview_source_tile_ids();
        let Some(layout) = self.overview_layout(&source_tile_ids) else {
            return;
        };
        // REQ-OV-14: keep the selection in range as source panes come and go.
        if let Some(overview) = self.overview_window.as_mut() {
            overview.selected = overview
                .selected
                .min(source_tile_ids.len().saturating_sub(1));
        }
        let now = Instant::now();
        let due_tile_ids = self.due_overview_tile_ids(&source_tile_ids, now);

        self.ensure_overview_thumbnails(&layout);
        self.render_due_overview_tiles(&due_tile_ids, &source_tile_ids);
        self.render_due_overview_title_bands(&due_tile_ids, &source_tile_ids, &layout);
        self.render_overview_placeholder_labels(&source_tile_ids, &layout);
        self.present_overview_frame(&layout);

        self.finish_overview_tile_renders(&due_tile_ids, now);

        // OVERVIEW_MAX_RENDER_TILES_PER_FRAME caps how many tiles one frame
        // regenerates, and idle tabs produce no pty output to trigger the
        // next frame — so a dirty backlog can survive this frame for two
        // different reasons, and only one of them justifies re-requesting a
        // redraw right away (Fix A): a due-but-capped tile (immediate), vs.
        // a tile that is merely inside its 10Hz throttle window (schedule
        // one delayed wake-up via `tick_overview_backlog` instead of
        // spinning `present_overview_frame` until it's due).
        let candidates = self.overview_tile_candidates(&source_tile_ids);
        let decision =
            overview_backlog_decision(&candidates, now, OVERVIEW_TILE_MIN_RENDER_INTERVAL);
        if decision.request_immediate_redraw {
            self.overview_wake_deadline = None;
            self.request_overview_redraw();
        } else {
            self.overview_wake_deadline = decision.wake_at;
        }
    }

    pub(super) fn focus_overview_tile_at_last_cursor(&mut self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        let Some(point) = overview.last_cursor_point else {
            return;
        };

        let source_tile_ids = self.overview_source_tile_ids();
        let Some(layout) = self.overview_layout(&source_tile_ids) else {
            return;
        };
        let Some(target) = overview_tile_target_at_point(&source_tile_ids, &layout.tiles, point)
        else {
            return;
        };
        // The clicked tile becomes the selection too, not just the focus
        // target — a click and an arrow-keyed Return should leave the
        // Overview in the same selected state.
        if let Some(index) = source_tile_ids.iter().position(|id| *id == target)
            && let Some(overview) = self.overview_window.as_mut()
        {
            overview.selected = index;
        }
        self.focus_tile_from_overview(target);
    }

    /// The close-button (✕) target under the last cursor point, or `None`
    /// (REQ-OV-13). Spans live tiles and placeholder rows — both carry a title
    /// bar with a close button, and both map back to a live source pane.
    pub(super) fn overview_close_target_at_last_cursor(&self) -> Option<OverviewTileId> {
        let overview = self.overview_window.as_ref()?;
        let point = overview.last_cursor_point?;
        let source_tile_ids = self.overview_source_tile_ids();
        let layout = self.overview_layout(&source_tile_ids)?;
        let tile_rects: Vec<PaneRectApp> = layout
            .tiles
            .iter()
            .chain(layout.placeholders.iter())
            .copied()
            .collect();
        overview_close_target_at_point(&source_tile_ids, &tile_rects, point)
    }

    pub(super) fn focus_tile_from_overview(&mut self, tile_id: OverviewTileId) {
        let Some(window) = self
            .windows
            .get(&tile_id.window_id)
            .map(|state| state.window.clone())
        else {
            return;
        };
        self.focus_pane(tile_id.window_id, tile_id.pane_id);
        self.focused = Some(tile_id.window_id);
        window.focus_window();
    }

    /// Drives the Overview-focused keymap directly from the keypress
    /// (REQ-OV-15), mirroring `handle_search_prompt_key`'s
    /// keypress-interception shape: arrows/Return/Esc/Cmd+1..9 are resolved
    /// here and never reach `handle_app_command`, so they can't be swallowed
    /// by `overview_command_scope`'s blanket `AppCommand` no-op. Every other
    /// key falls through to the normal keybind-resolve path, which still
    /// classifies terminal commands as Overview no-ops (REQ-OV-7).
    pub(super) fn handle_overview_key(&mut self, event_loop: &ActiveEventLoop, event: &KeyEvent) {
        if let Some(action) = overview_key_action(&event.logical_key, self.modifiers) {
            match action {
                OverviewAction::MoveSelection(direction) => self.step_overview_selection(direction),
                OverviewAction::Activate => self.activate_overview_selection(),
                OverviewAction::SwitchToLive(n) => self.switch_to_live_overview_tile(n),
                OverviewAction::Dismiss => self.dismiss_or_clear_overview_search(),
                OverviewAction::ToggleZoom => self.toggle_overview_zoom(),
            }
            return;
        }
        // Printable text / Backspace edits the "Search sessions" query (REQ-OV-16),
        // slotted after the Overview action keymap (arrows/Return/Esc/Cmd+N win)
        // and before the normal keybind fallthrough. Nothing here reaches a pty
        // (REQ-OV-7).
        if self.apply_overview_search_edit(event) {
            return;
        }
        if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
            self.handle_app_command(event_loop, command, CommandOrigin::OverviewWindow);
        }
    }

    /// Escape while the Overview is focused (REQ-OV-16): a non-empty search
    /// query is cleared first and the Overview stays open, an empty query
    /// dismisses it (two-stage Escape; no command-palette precedent, so the
    /// semantics are defined by `overview_escape_action`).
    pub(super) fn dismiss_or_clear_overview_search(&mut self) {
        let query = self
            .overview_window
            .as_ref()
            .map_or("", |overview| overview.search_query.as_str());
        match overview_escape_action(query) {
            OverviewEscapeAction::ClearSearch => self.set_overview_search_query(String::new()),
            OverviewEscapeAction::Dismiss => self.hide_tab_overview(),
        }
    }

    /// Apply a printable-text append or Backspace pop to the "Search sessions"
    /// query (REQ-OV-16). Returns `true` when the key was consumed as a query
    /// edit. Cmd/Ctrl/Alt combos are not swallowed here (they fall through to
    /// the keybind path, mirroring the command palette's Cmd-swallow), so e.g.
    /// the Overview toggle chord still works while typing.
    pub(super) fn apply_overview_search_edit(&mut self, event: &KeyEvent) -> bool {
        let Some(mut query) = self
            .overview_window
            .as_ref()
            .map(|overview| overview.search_query.clone())
        else {
            return false;
        };
        match &event.logical_key {
            Key::Named(NamedKey::Backspace) => {
                if query.pop().is_none() {
                    // Already empty: still consumed (Backspace has no other
                    // meaning in the Overview) but no redraw is needed.
                    return true;
                }
            }
            _ => {
                if self.modifiers.super_key()
                    || self.modifiers.control_key()
                    || self.modifiers.alt_key()
                {
                    return false;
                }
                let Some(text) = event.text.as_deref() else {
                    return false;
                };
                let mut appended = false;
                for c in text.chars().filter(|c| !c.is_control()) {
                    query.push(c);
                    appended = true;
                }
                if !appended {
                    return false;
                }
            }
        }
        self.set_overview_search_query(query);
        true
    }

    /// Replace the search query, reset the selection to the first tile (a
    /// query change re-orders the result set, REQ-OV-16 / palette R-7 parity),
    /// and request a redraw.
    pub(super) fn set_overview_search_query(&mut self, query: String) {
        if let Some(overview) = self.overview_window.as_mut() {
            overview.search_query = query;
            overview.selected = 0;
        } else {
            return;
        }
        // Filtering remaps each window to a new tile slot, so the now-visible
        // set must re-render into those slots instead of showing the previous
        // ordering's stale mirrors. Re-rendering still flows through the 10Hz
        // throttle (REQ-NF-4), so tiles refresh at the next due tick.
        self.mark_all_overview_tiles_dirty();
        self.request_overview_redraw();
    }

    /// Arrow-key Overview selection move (REQ-OV-15a).
    pub(super) fn step_overview_selection(&mut self, direction: Direction) {
        let source_tile_ids = self.overview_source_tile_ids();
        let Some(layout) = self.overview_layout(&source_tile_ids) else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        overview.selected = move_overview_selection(
            overview.selected,
            layout.cols,
            source_tile_ids.len(),
            direction,
        );
        overview.window.request_redraw();
    }

    /// Tab toggles a quick-look zoom of the selected tile: the tile's card is
    /// re-composited enlarged and centered above the grid. Purely visual — the
    /// selection, hit-testing, and keyboard nav are unaffected.
    pub(super) fn toggle_overview_zoom(&mut self) {
        if let Some(overview) = self.overview_window.as_mut() {
            overview.zoomed = !overview.zoomed;
            overview.window.request_redraw();
        }
    }

    /// Recompute which tile (live or placeholder, in source order) the cursor
    /// is over, and repaint on a change so the hover accent ring tracks the
    /// mouse. Pure math per mouse move; no GPU work unless it changed.
    pub(super) fn update_overview_hover(&mut self) {
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        let point = overview.last_cursor_point;
        let source_tile_ids = self.overview_source_tile_ids();
        let hovered = point.and_then(|point| {
            let layout = self.overview_layout(&source_tile_ids)?;
            layout
                .tiles
                .iter()
                .chain(layout.placeholders.iter())
                .position(|rect| rect.contains(point))
        });
        if let Some(overview) = self.overview_window.as_mut()
            && overview.hovered != hovered
        {
            overview.hovered = hovered;
            overview.window.request_redraw();
        }
    }

    /// The status-dot color for a tile's title band, mirroring the sidebar's
    /// dot semantics (FR-11/FR-16): red while the attention marker is in its
    /// visible blink phase, else yellow for an unread bell, blue for a busy
    /// program, and `None` for idle (no dot). During the attention blink's
    /// hidden phase the underlying bell/busy color shows, so the band blinks
    /// in phase with the sidebar (FR-A2).
    pub(super) fn overview_tile_dot_color(&self, tile_id: OverviewTileId) -> Option<noa_core::Rgb> {
        let card_id = Self::session_card_id(tile_id.window_id, tile_id.pane_id);
        let card = self.session_store.get(&card_id)?;
        if card.attention && self.attention_marker_visible(&card_id) {
            Some(crate::chrome::CHROME_DOT_RED)
        } else if card.unread_bell {
            Some(crate::chrome::CHROME_DOT_YELLOW)
        } else if card.busy {
            Some(crate::chrome::CHROME_DOT_BLUE)
        } else {
            None
        }
    }

    /// Return activates the selected Overview tile (REQ-OV-15b). `selected`
    /// indexes directly into the combined live + placeholder source order,
    /// so a selected placeholder row resolves to its source pane exactly the
    /// same way a selected live tile does.
    pub(super) fn activate_overview_selection(&mut self) {
        let source_tile_ids = self.overview_source_tile_ids();
        let Some(overview) = self.overview_window.as_ref() else {
            return;
        };
        let Some(&target) = source_tile_ids.get(overview.selected) else {
            return;
        };
        self.focus_tile_from_overview(target);
    }

    /// Cmd+`n` (1-indexed) jumps straight to the `n`-th live Overview tile
    /// (REQ-OV-15c). Out-of-range `n` (beyond the live tile count) is a
    /// no-op rather than a panic — there is no tile to switch to.
    pub(super) fn switch_to_live_overview_tile(&mut self, n: usize) {
        let source_tile_ids = self.overview_source_tile_ids();
        let live_tile_count = OVERVIEW_GRID_CAP.min(source_tile_ids.len());
        if n == 0 || n > live_tile_count {
            return;
        }
        let target = source_tile_ids[n - 1];
        if let Some(overview) = self.overview_window.as_mut() {
            overview.selected = n - 1;
        }
        self.focus_tile_from_overview(target);
    }

    pub(super) fn overview_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if !self.is_overview_window(window_id) {
            return;
        }

        match event {
            WindowEvent::CloseRequested => self.hide_tab_overview(),
            WindowEvent::RedrawRequested => self.redraw_overview(),
            WindowEvent::Resized(size) => self.on_overview_resize(size),
            WindowEvent::CursorMoved { position, .. } => {
                let point = split_point_from_physical_position(position);
                if let Some(overview) = self.overview_window.as_mut() {
                    overview.last_cursor_point = point;
                }
                self.update_overview_hover();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left && state == ElementState::Pressed {
                    // REQ-OV-13: the close-button corner wins over tile-focus.
                    // Close the targeted pane; `close_pane` falls back to
                    // closing the tab when it was the last pane.
                    if let Some(target) = self.overview_close_target_at_last_cursor() {
                        self.hide_tab_overview();
                        if let Some(state) = self.windows.get(&target.window_id) {
                            state.window.focus_window();
                        }
                        self.focused = Some(target.window_id);
                        self.request_close_pane(event_loop, target.window_id, target.pane_id);
                    } else {
                        self.focus_overview_tile_at_last_cursor();
                    }
                }
            }
            WindowEvent::Occluded(occluded) => {
                if let Some(overview) = self.overview_window.as_mut() {
                    overview.occluded = occluded;
                    if !occluded && self.overview_visible {
                        overview.window.request_redraw();
                    }
                }
            }
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                self.handle_overview_key(event_loop, &event);
            }
            _ => {}
        }
    }
}
