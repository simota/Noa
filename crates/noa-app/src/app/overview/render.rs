use super::super::*;

impl App {
    pub(in crate::app) fn ensure_overview_thumbnails(&mut self, layout: &OverviewLayout) {
        let Some(host_config) = self.overview_host_surface_config() else {
            return;
        };
        let Some(metrics) = self.overview_metrics() else {
            return;
        };
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
            w: host_config.width,
            h: host_config.height,
        };
        let format = host_config.format;

        let stale = overview.thumbnails.as_ref().is_none_or(|thumbnails| {
            thumbnails.format() != format
                || thumbnails.tile_size() != tile_size
                || thumbnails.tile_count() != tile_count
        });
        if stale {
            overview.thumbnails = Some(OverviewThumbnailResources::new(
                &gpu.device,
                &gpu.queue,
                format,
                scratch_size,
                tile_size,
                tile_count,
                metrics.title_bar_h,
                overview_card_color(),
            ));
        }
    }

    /// Render each due tile's source pane into the shared scratch texture and
    /// blit it down into that pane's tile texture (REQ-OV-4 live mirror,
    /// REQ-NF-1 reuse the tab's own `Renderer`, REQ-NF-3 shared-scratch
    /// blit-downscale). `tile_index` is `source_tile_ids`' position, which
    /// is index-parallel with `layout.tiles` (see `overview_tile_target_at_point`).
    pub(in crate::app) fn render_due_overview_tiles(
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
            state.renderer.rebuild_cells(
                &snapshot,
                &mut gpu.font,
                active_theme(&gpu.theme, &gpu.preview_theme),
            );
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
    ///
    /// Labels render in the dedicated `sidebar_font` (smaller, denser, DPR-
    /// scaled) rather than the user's terminal font — the bands are UI chrome
    /// sized in design px, sidebar parity. The construction padding is a
    /// placeholder: every band draw sets its own centering padding via
    /// `set_grid_padding` before drawing.
    pub(in crate::app) fn ensure_overview_label_renderer(&mut self) {
        let Some(format) = self
            .overview_host_surface_config()
            .map(|config| config.format)
        else {
            return;
        };
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
        let stale = overview
            .label_renderer
            .as_ref()
            .is_none_or(|renderer| renderer.target_format() != format);
        if stale {
            overview.label_renderer = Some(
                Renderer::new(
                    &gpu.device,
                    &gpu.queue,
                    format,
                    &mut gpu.sidebar_font,
                    GridPadding::ZERO,
                )
                .expect("failed to build the overview label renderer"),
            );
        }
    }

    pub(in crate::app) fn ensure_overview_chrome_card_pipeline(&mut self) {
        let Some(format) = self
            .overview_host_surface_config()
            .map(|config| config.format)
        else {
            return;
        };
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };
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
    pub(in crate::app) fn render_due_overview_title_bands(
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
    pub(in crate::app) fn render_overview_placeholder_labels(
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
    pub(in crate::app) fn render_tile_title_band(
        &mut self,
        tile_index: usize,
        title: &str,
        badge: Option<usize>,
        dot: Option<noa_core::Rgb>,
        query: &str,
    ) {
        self.ensure_overview_label_renderer();
        let Some(metrics) = self.overview_metrics() else {
            return;
        };
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
        let padding = overview_label_padding(
            band_size.h,
            gpu.sidebar_font.metrics().cell_h,
            metrics.scale(),
        );
        let grid_size = grid_size_for_pane_rect(
            PaneRectApp::new(0, 0, band_size.w, band_size.h),
            gpu.sidebar_font.metrics(),
            padding,
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
        label_renderer.set_grid_padding(padding);
        label_renderer.rebuild_cells(
            &snapshot,
            &mut gpu.sidebar_font,
            active_theme(&gpu.theme, &gpu.preview_theme),
        );
        // After `rebuild_cells` (which resets it from the snapshot bg) so the
        // band gets its distinct title-bar color, not the terminal default.
        label_renderer.set_clear_color(overview_title_bar_color());
        label_renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.sidebar_font);

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
    pub(in crate::app) fn render_overview_search_texture(
        &mut self,
    ) -> Option<OverviewChromeTexture> {
        let format = self.overview_host_surface_config()?.format;
        let metrics = self.overview_metrics()?;
        let chrome = self.overview_chrome()?;
        let rect = overview_search_field_rect(chrome.search_band, metrics);
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
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = search_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let padding = overview_label_padding(
            band_size.h,
            gpu.sidebar_font.metrics().cell_h,
            metrics.scale(),
        );
        let grid_size = grid_size_for_pane_rect(
            PaneRectApp::new(0, 0, band_size.w, band_size.h),
            gpu.sidebar_font.metrics(),
            padding,
        );
        let text = overview_search_field_row(&query, grid_size.cols);
        let mut term = Terminal::new(GridSize::new(grid_size.cols, 1));
        Stream::new().feed(text.as_bytes(), &mut term);
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;

        label_renderer.resize(band_size);
        label_renderer.set_grid_padding(padding);
        label_renderer.rebuild_cells(
            &snapshot,
            &mut gpu.sidebar_font,
            active_theme(&gpu.theme, &gpu.preview_theme),
        );
        label_renderer.set_clear_color(overview_chrome_pill_color());
        label_renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.sidebar_font);
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
    pub(in crate::app) fn render_overview_hint_texture(
        &mut self,
        live_tile_count: usize,
    ) -> Option<OverviewChromeTexture> {
        let format = self.overview_host_surface_config()?.format;
        let metrics = self.overview_metrics()?;
        let chrome = self.overview_chrome()?;
        let rect = overview_hint_bar_rect(chrome.hint_band, metrics);
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
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = hint_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let padding = overview_label_padding(
            band_size.h,
            gpu.sidebar_font.metrics().cell_h,
            metrics.scale(),
        );
        let grid_size = grid_size_for_pane_rect(
            PaneRectApp::new(0, 0, band_size.w, band_size.h),
            gpu.sidebar_font.metrics(),
            padding,
        );
        let text = overview_hint_bar_row(live_tile_count, grid_size.cols);
        let mut term = Terminal::new(GridSize::new(grid_size.cols, 1));
        Stream::new().feed(text.as_bytes(), &mut term);
        let mut snapshot = FrameSnapshot::from_terminal(&mut term);
        snapshot.cursor.visible = false;

        label_renderer.resize(band_size);
        label_renderer.set_grid_padding(padding);
        label_renderer.rebuild_cells(
            &snapshot,
            &mut gpu.sidebar_font,
            active_theme(&gpu.theme, &gpu.preview_theme),
        );
        label_renderer.set_clear_color(overview_chrome_pill_color());
        label_renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.sidebar_font);
        label_renderer.draw(&gpu.device, &gpu.queue, &view);

        Some(OverviewChromeTexture {
            texture: hint_texture,
            rect,
        })
    }

    /// Composite every live-mirror and placeholder tile onto the overview
    /// surface as a rounded card (REQ-OV-12/14), then overlay the bottom hint
    /// bar (REQ-OV-17), and present. Empty grid cells stay the backdrop color.
    pub(in crate::app) fn present_overview_frame(&mut self, layout: &OverviewLayout) {
        // Render the hint band first (it borrows the label renderer / gpu
        // mutably); the returned texture is owned, so the borrows are released
        // before compositing.
        let live_count = layout.tiles.len();
        let search_texture = self.render_overview_search_texture();
        let hint_texture = self.render_overview_hint_texture(live_count);
        self.ensure_overview_chrome_card_pipeline();
        let Some(metrics) = self.overview_metrics() else {
            return;
        };
        let (selected, hovered, zoomed, zoom_anim) =
            self.overview_window
                .as_ref()
                .map_or((0, None, false, None), |overview| {
                    (
                        overview.selected,
                        overview.hovered,
                        overview.zoomed,
                        overview.zoom_anim,
                    )
                });
        // The quick-look zoom eases between the tile's grid rect (factor 0)
        // and its enlarged centered rect (factor 1); a finished expand holds
        // at 1, a finished collapse at 0.
        let now = Instant::now();
        let zoom_factor = match zoom_anim {
            Some(anim) => {
                let p = anim.tween.progress(now);
                if anim.expanding { p } else { 1.0 - p }
            }
            None if zoomed => 1.0,
            None => 0.0,
        };
        // The zoom overlay centers within the grid band; resolved before the
        // gpu/overview borrows below.
        let zoom_bounds = self.overview_chrome().map(|chrome| chrome.grid_bounds);

        // Which tiles carry a pending interaction request (FR-16), indexed by
        // placement position (live tiles then placeholders, the same order as
        // `tile_rects` below). Resolved before the gpu/overview borrows so the
        // ring pass needs no `self` access.
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

        let Some(host) = self.overview_host() else {
            return;
        };
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(host_state) = self.windows.get(&host) else {
            return;
        };
        let Some(overview) = self.overview_window.as_mut() else {
            return;
        };

        // The overlay presents into the host window's own surface — the same
        // one the terminal frame uses when the Overview is hidden.
        let frame = match host_state.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                host_state
                    .surface
                    .configure(&gpu.device, &host_state.surface_config);
                host_state.window.request_redraw();
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
            w: host_state.surface_config.width,
            h: host_state.surface_config.height,
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
                &overview_card_style(metrics),
                &placements,
            );
        } else {
            // No tiles: still clear the surface to the backdrop color.
            clear_overview_surface(&gpu.device, &gpu.queue, &view, overview_bg_color());
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
                &overview_chrome_card_style(metrics),
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
                    focus_width: crate::chrome::RING_HOVER * metrics.scale(),
                    focus_glow_width: 0.0,
                    ..overview_card_style(metrics)
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
            // centered within the grid band with the full selection ring —
            // eased between the grid rect and the zoom rect while the
            // quick-look transition runs.
            if zoom_factor > 0.0
                && let Some(bounds) = zoom_bounds
                && let Some(rect) = tile_rects.get(selected)
                && let Some(tile_view) = thumbnails.tile_texture_view(selected)
            {
                let target = overview_zoom_rect(bounds, *rect);
                let lerp_dim = |a: u32, b: u32| {
                    crate::anim::lerp(a as f32, b as f32, zoom_factor)
                        .round()
                        .max(0.0) as u32
                };
                chrome_card.pipeline.overlay_texture_cards(
                    &gpu.device,
                    &gpu.queue,
                    &view,
                    surface_size,
                    &overview_card_style(metrics),
                    &[CardTexturePlacement {
                        texture_view: &tile_view,
                        x: lerp_dim(rect.x, target.x),
                        y: lerp_dim(rect.y, target.y),
                        w: lerp_dim(rect.w, target.w),
                        h: lerp_dim(rect.h, target.h),
                        selected: true,
                    }],
                );
            }

            // Persistent red attention ring over non-focused/non-hovered tiles
            // with a pending interaction request (FR-16). Focus and hover keep
            // the blue accent ring; the title-band dot still marks attention,
            // so hovering an attention tile never turns the hover affordance
            // pink/red.
            for (index, rect) in tile_rects.iter().enumerate() {
                if !overview_attention_ring_visible(
                    attention_tiles.get(index).copied().unwrap_or(false),
                    index,
                    selected,
                    hovered,
                ) {
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
                    &overview_attention_card_style(metrics),
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

        // Drive the quick-look zoom transition: repaint until it settles,
        // then drop the tween so the steady state draws without it.
        if let Some(anim) = overview.zoom_anim {
            if anim.tween.done(now) {
                overview.zoom_anim = None;
            } else {
                host_state.window.request_redraw();
            }
        }
    }

    pub(in crate::app) fn finish_overview_tile_renders(
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
    pub(in crate::app) fn redraw_overview(&mut self) {
        if self.overview_window.is_none() {
            return;
        }
        if !self.overview_visible || self.overview_window_occluded() {
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
}

fn overview_attention_ring_visible(
    attention: bool,
    index: usize,
    selected: usize,
    hovered: Option<usize>,
) -> bool {
    attention && index != selected && hovered != Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_attention_ring_does_not_override_selected_or_hovered_tiles() {
        assert!(overview_attention_ring_visible(true, 2, 0, None));
        assert!(!overview_attention_ring_visible(true, 2, 2, None));
        assert!(!overview_attention_ring_visible(true, 2, 0, Some(2)));
        assert!(!overview_attention_ring_visible(false, 2, 0, None));
    }
}
