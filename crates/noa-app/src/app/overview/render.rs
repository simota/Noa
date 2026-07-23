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
        due_tab_ids: &[WindowId],
        source_tab_ids: &[WindowId],
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

        // The tile-local content region (below the title band) every tab's
        // panes composite into — one uniform tile size for the whole grid.
        let tile_size = thumbnails.tile_size();
        let content = crate::session_overview::tab_tile_content_rect(
            PaneRectApp::new(0, 0, tile_size.w, tile_size.h),
            thumbnails.title_bar_h(),
        );

        for &window_id in due_tab_ids {
            let Some(tile_index) = source_tab_ids.iter().position(|id| *id == window_id) else {
                continue;
            };
            let Some(state) = self.windows.get_mut(&window_id) else {
                continue;
            };
            // Overview U1: lay the tab's SplitTree into the tile content region
            // (scaled), so the tile reproduces the tab's real internal split
            // geometry — each pane gets a sub-rect to composite its mirror into.
            let pane_rects =
                crate::session_overview::tab_tile_pane_rects(content, &state.split_tree);

            // Clear the whole tab tile to the card color first, so divider gaps
            // and any pane still lacking a published mirror read as an empty
            // card rather than stale/uninitialized pixels; then composite each
            // pane on top without re-clearing.
            thumbnails.clear_tile(&gpu.device, &gpu.queue, tile_index);

            // Reuse this tab's own `Renderer` unmodified (REQ-NF-1): point it at
            // each source pane's real pixel size just long enough to draw one
            // frame into the Overview scratch texture, then restore its real
            // surface viewport so the tab's own next redraw is unaffected.
            let own_viewport = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            let mut rendered_any = false;
            for (pane_id, sub_rect) in pane_rects {
                let Some(surface) = state.surfaces.get(&pane_id) else {
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
                // locks a tab's `Terminal` itself. `None` only for a pane that
                // hasn't published since the overview opened;
                // `seed_overview_snapshots`'s one-time fallback covers that gap
                // (the pane's sub-rect stays the card-color clear until then).
                let Some(snapshot) = surface.overview_snapshot.lock().clone() else {
                    continue;
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
                rendered_any = true;
                if let Err(err) = thumbnails.render_pane_into_tile_subrect(
                    &gpu.device,
                    &gpu.queue,
                    &mut state.renderer,
                    source_viewport,
                    tile_index,
                    (sub_rect.x, sub_rect.y, sub_rect.w, sub_rect.h),
                ) {
                    log::warn!(
                        "overview tile render failed for {window_id:?}/pane {}: {err:#}",
                        pane_id.get()
                    );
                }
            }
            if rendered_any {
                state.renderer.resize(own_viewport);
            }
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
            let pipelines = gpu.pipelines.get(&gpu.device, format);
            let sidebar_font_atlases =
                gpu.sidebar_font_atlases
                    .get(&gpu.device, &gpu.queue, format, &gpu.sidebar_font);
            overview.label_renderer = Some(
                Renderer::with_pipelines(
                    &gpu.device,
                    &gpu.queue,
                    &pipelines,
                    &sidebar_font_atlases,
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
        due_tab_ids: &[WindowId],
        source_tab_ids: &[WindowId],
        layout: &OverviewLayout,
    ) {
        let live_count = layout.tiles.len().min(source_tab_ids.len());
        let live_ids = &source_tab_ids[..live_count];
        let labels = overview_tile_labels(live_ids, |id| self.overview_tab_label(id));
        let query = self
            .overview_window
            .as_ref()
            .map_or(String::new(), |overview| overview.search_query.clone());

        // Each live tab tile band carries its `⌘n` switch badge (REQ-OV-15c,
        // only the 1..=9 the keymap reaches), its aggregate status-dot color,
        // and the live search query for match highlighting (REQ-OV-16).
        let jobs: Vec<(usize, String, Option<usize>, Option<noa_core::Rgb>)> = labels
            .iter()
            .enumerate()
            .filter(|(index, _)| due_tab_ids.contains(&live_ids[*index]))
            .map(|(index, label)| {
                (
                    index,
                    label.label.clone(),
                    (index < 9).then_some(index + 1),
                    self.overview_tab_dot_color(live_ids[index]),
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
        source_tab_ids: &[WindowId],
        layout: &OverviewLayout,
    ) {
        // v3 paging never overflows a page (each page holds ≤ OVERVIEW_GRID_CAP
        // tabs, so `compute_overview_grid` yields no placeholder rows) — this
        // stays as a defensive no-op for the degenerate over-cap case, now
        // labelling any overflow tab tile with its own tab title.
        if layout.placeholders.is_empty() {
            return;
        }
        let live_count = layout.tiles.len();
        let overflow_ids = overview_placeholder_source_ids(source_tab_ids, live_count);
        let labels = overview_tile_labels(overflow_ids, |id| self.overview_tab_label(id));
        let query = self
            .overview_window
            .as_ref()
            .map_or(String::new(), |overview| overview.search_query.clone());

        let jobs: Vec<(usize, String, Option<noa_core::Rgb>)> = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let window_id = overflow_ids[index];
                (
                    live_count + index,
                    label.label.clone(),
                    self.overview_tab_dot_color(window_id),
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

    /// Column/row grid a `band_size`-sized chrome/tile-title raster resolves
    /// to in the shared label `Renderer`'s (smaller, denser sidebar) font —
    /// shared by the tile title band, search pill, and hint pill so each
    /// caller's text truncation (`sanitize_placeholder_label`, the `..._row`
    /// helpers' own clipping) uses the exact column count the label renderer
    /// will actually draw into.
    pub(in crate::app) fn overview_label_grid(
        &self,
        band_size: PixelSize,
    ) -> Option<(GridPadding, GridSize)> {
        let metrics = self.overview_metrics()?;
        let gpu = self.gpu.as_ref()?;
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
        Some((padding, grid_size))
    }

    /// Shared text-to-texture raster core (REQ-OV-12/16/17): feeds
    /// `text` — already ANSI-styled and column-clipped by the caller against
    /// `overview_label_grid`'s `cols` — through a one-row scratch `Terminal`,
    /// then draws it with the shared label `Renderer` into `target`. Every
    /// overview text raster (tile title band, search pill, hint pill)
    /// funnels through this single point, so the search/hint pill cache
    /// (`render_overview_search_texture` / `render_overview_hint_texture`)
    /// has one raster implementation to keep in sync instead of three.
    pub(in crate::app) fn draw_overview_label(
        &mut self,
        band_size: PixelSize,
        padding: GridPadding,
        cols: u16,
        clear_color: [f32; 4],
        text: &str,
        target: &wgpu::TextureView,
    ) -> Option<()> {
        self.ensure_overview_label_renderer();
        let gpu = self.gpu.as_mut()?;
        let overview = self.overview_window.as_mut()?;
        let label_renderer = overview.label_renderer.as_mut()?;

        let mut term = Terminal::new(GridSize::new(cols, 1));
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
        // target gets its own distinct backdrop, not the terminal default.
        label_renderer.set_clear_color(clear_color);
        label_renderer.sync_atlas(&gpu.device, &gpu.queue, &mut gpu.sidebar_font);
        label_renderer.draw(&gpu.device, &gpu.queue, target);
        Some(())
    }

    /// Render `title` into `tile_index`'s dedicated title-band texture via the
    /// shared label `Renderer`, then stamp it onto the top `OVERVIEW_TITLE_BAR_H`
    /// rows of the tile (REQ-OV-12). The band is cleared to a distinct
    /// title-bar color so it reads as a band separate from the card face.
    /// Shared by live and placeholder tiles. `badge` prepends the dim `⌘n`
    /// switch number, `dot` colors the label's `● ` needs-user prefix, and
    /// `query`'s first match inside the label is accent-highlighted
    /// (REQ-OV-15c/16, sidebar-parity dots). Not cached: the caller
    /// (`render_due_overview_title_bands` / `render_overview_placeholder_labels`)
    /// already only invokes this for tiles that are due this frame.
    pub(in crate::app) fn render_tile_title_band(
        &mut self,
        tile_index: usize,
        title: &str,
        badge: Option<usize>,
        dot: Option<noa_core::Rgb>,
        query: &str,
    ) {
        let Some((band_size, view)) = self.overview_window.as_ref().and_then(|overview| {
            let thumbnails = overview.thumbnails.as_ref()?;
            let tile_w = thumbnails.tile_size().w;
            let bar_h = thumbnails.title_bar_h();
            if tile_w == 0 || bar_h == 0 {
                return None;
            }
            let band_size = PixelSize {
                w: tile_w.max(1),
                h: bar_h.max(1),
            };
            let view = thumbnails.title_texture_view(tile_index)?;
            Some((band_size, view))
        }) else {
            return;
        };
        let Some((padding, grid_size)) = self.overview_label_grid(band_size) else {
            return;
        };
        let sanitized = sanitize_placeholder_label(title, grid_size.cols);
        // REQ-OV-13: the centered title plus a close glyph in the last column,
        // with inline SGR styling (badge / dot / search highlight).
        let text = title_bar_row_ansi(&sanitized, grid_size.cols, badge, dot, query);
        if self
            .draw_overview_label(
                band_size,
                padding,
                grid_size.cols,
                overview_title_bar_color(),
                &text,
                &view,
            )
            .is_none()
        {
            return;
        }
        if let (Some(gpu), Some(thumbnails)) = (
            self.gpu.as_ref(),
            self.overview_window
                .as_ref()
                .and_then(|overview| overview.thumbnails.as_ref()),
        ) {
            thumbnails.stamp_title_band(&gpu.device, &gpu.queue, tile_index);
        }
    }

    /// Render (or reuse from cache) the top "Search sessions" field
    /// (REQ-OV-16) as a pill-sized texture for compositing into the reserved
    /// top search band. Shows the live query, or the placeholder while it is
    /// empty. `None` when there is no usable search band (a window too short
    /// to reserve one).
    ///
    /// Cached on `OverviewWindowState.search_pill_cache`, keyed by
    /// `OverviewPillKey` (query text, live tile count, rect) — a hover-only
    /// redraw changes none of these, so it reuses the last raster instead of
    /// minting a fresh GPU texture every frame.
    pub(in crate::app) fn render_overview_search_texture(
        &mut self,
        live_tile_count: usize,
        page: usize,
    ) -> Option<OverviewChromeTexture> {
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
        let key = OverviewPillKey {
            query: query.clone(),
            live_tile_count,
            page,
            rect,
        };
        if let Some(hit) = self
            .overview_window
            .as_ref()
            .and_then(|overview| overview_pill_cache_hit(overview.search_pill_cache.as_ref(), &key))
        {
            return Some(hit.clone());
        }

        let format = self.overview_host_surface_config()?.format;
        let band_size = PixelSize {
            w: rect.w.max(1),
            h: rect.h.max(1),
        };
        let (padding, grid_size) = self.overview_label_grid(band_size)?;
        let text = overview_search_field_row(&query, grid_size.cols);

        let texture = self
            .gpu
            .as_ref()?
            .device
            .create_texture(&wgpu::TextureDescriptor {
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
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.draw_overview_label(
            band_size,
            padding,
            grid_size.cols,
            overview_chrome_pill_color(),
            &text,
            &view,
        )?;

        let chrome_texture = OverviewChromeTexture { view, rect };
        if let Some(overview) = self.overview_window.as_mut() {
            overview.search_pill_cache = Some((key, chrome_texture.clone()));
        }
        Some(chrome_texture)
    }

    /// Render (or reuse from cache) the bottom hint bar (REQ-OV-17) as a
    /// pill-sized texture for compositing onto the surface. `None` when
    /// there is no usable hint band (a window too short to reserve one).
    /// The `⌘1-N` range tracks the live tile count dynamically, and a
    /// trailing "Page p/N" segment appears whenever `page_count > 1` (v3
    /// paging, REQ-OV-19).
    ///
    /// Cached on `OverviewWindowState.hint_pill_cache` the same way as the
    /// search pill (see `render_overview_search_texture`) — `page` is folded
    /// into the shared [`OverviewPillKey`] so a page flip invalidates it.
    pub(in crate::app) fn render_overview_hint_texture(
        &mut self,
        live_tile_count: usize,
        page: usize,
        page_count: usize,
    ) -> Option<OverviewChromeTexture> {
        let metrics = self.overview_metrics()?;
        let chrome = self.overview_chrome()?;
        let rect = overview_hint_bar_rect(chrome.hint_band, metrics);
        if rect.w == 0 || rect.h == 0 {
            return None;
        }
        let query = self
            .overview_window
            .as_ref()
            .map_or(String::new(), |overview| overview.search_query.clone());
        let key = OverviewPillKey {
            query,
            live_tile_count,
            page,
            rect,
        };
        if let Some(hit) = self
            .overview_window
            .as_ref()
            .and_then(|overview| overview_pill_cache_hit(overview.hint_pill_cache.as_ref(), &key))
        {
            return Some(hit.clone());
        }

        let format = self.overview_host_surface_config()?.format;
        let band_size = PixelSize {
            w: rect.w.max(1),
            h: rect.h.max(1),
        };
        let (padding, grid_size) = self.overview_label_grid(band_size)?;
        let text = overview_hint_bar_row(live_tile_count, page, page_count, grid_size.cols);

        let texture = self
            .gpu
            .as_ref()?
            .device
            .create_texture(&wgpu::TextureDescriptor {
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
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.draw_overview_label(
            band_size,
            padding,
            grid_size.cols,
            overview_chrome_pill_color(),
            &text,
            &view,
        )?;

        let chrome_texture = OverviewChromeTexture { view, rect };
        if let Some(overview) = self.overview_window.as_mut() {
            overview.hint_pill_cache = Some((key, chrome_texture.clone()));
        }
        Some(chrome_texture)
    }

    /// Composite every live tile of the current page onto the overview
    /// surface as a rounded card (REQ-OV-12/14, v3 paging — a page never has
    /// placeholder rows), then overlay the bottom hint bar (REQ-OV-17,
    /// REQ-OV-19), and present. Empty grid cells stay the backdrop color.
    /// `source_tile_ids` is the current page's tile slice, index-parallel
    /// with `layout.tiles` (and thus with `tile_rects` below); `page`/
    /// `page_count` drive the hint bar's "Page p/N" segment.
    pub(in crate::app) fn present_overview_frame(
        &mut self,
        layout: &OverviewLayout,
        source_tab_ids: &[WindowId],
        page: usize,
        page_count: usize,
    ) {
        // Render the hint band first (it borrows the label renderer / gpu
        // mutably); the returned texture is owned, so the borrows are released
        // before compositing.
        let live_count = layout.tiles.len();
        let search_texture = self.render_overview_search_texture(live_count, page);
        let hint_texture = self.render_overview_hint_texture(live_count, page, page_count);
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
        // placement position — index-parallel with `source_tile_ids` (the
        // current page's slice) and thus with `tile_rects` below (v3 paging:
        // a page has no placeholder rows, so the two orders coincide
        // exactly). Resolved before the gpu/overview borrows so the ring
        // pass needs no `self` access.
        let attention_tiles: Vec<(bool, bool)> = source_tab_ids
            .iter()
            .map(|window_id| {
                // A tab tile wants attention if any pane does; its ring gets a
                // glow only during the one-shot arrival emphasis.
                self.overview_pane_ids_for_window(*window_id).iter().fold(
                    (false, false),
                    |(attention, emphasized), pane_id| {
                        let card_id = Self::session_card_id(*window_id, *pane_id);
                        let pane_attention = self
                            .session_store
                            .get(&card_id)
                            .is_some_and(|card| card.attention);
                        let pane_emphasized = pane_attention
                            && self
                                .attention_flash_until
                                .get(&card_id)
                                .is_some_and(|until| now < *until);
                        (attention || pane_attention, emphasized || pane_emphasized)
                    },
                )
            })
            .collect();

        // Overview pane-drag visuals: the source *tab*'s tile index (for the
        // floating chip), and the hovered tab's tile index when it is a *valid*
        // drop target. Resolved before the gpu/overview borrows below.
        let active_drag = self
            .overview_window
            .as_ref()
            .and_then(|overview| overview.pane_drag)
            .filter(|drag| drag.phase == PaneDragPhase::Active);
        let drag_source_index = active_drag.and_then(|drag| {
            source_tab_ids
                .iter()
                .position(|id| *id == drag.source.window_id)
        });
        // U4 floating chip: the dragged *pane*'s sub-rect of its tab tile
        // texture, as a normalized `src_uv` plus the sub-rect's pixel size (for
        // the chip's aspect). Resolved before the gpu/overview borrows so the
        // chip draw below needs no `self` access. The pane is composited into
        // this exact sub-rect of the tile by `render_due_overview_tiles`, so
        // sampling that sub-rect shows just the dragged pane, not the whole tab.
        let drag_chip: Option<([f32; 4], u32, u32)> = active_drag.and_then(|drag| {
            let index = drag_source_index?;
            let tile = layout.tiles.get(index)?;
            let content = crate::session_overview::tab_tile_content_rect(
                PaneRectApp::new(0, 0, tile.w, tile.h),
                metrics.title_bar_h,
            );
            let state = self.windows.get(&drag.source.window_id)?;
            let pane_rects =
                crate::session_overview::tab_tile_pane_rects(content, &state.split_tree);
            let (_, sub) = pane_rects
                .into_iter()
                .find(|(pane, _)| *pane == drag.source.pane_id)?;
            let tile_w = tile.w.max(1) as f32;
            let tile_h = tile.h.max(1) as f32;
            let src_uv = [
                sub.x as f32 / tile_w,
                sub.y as f32 / tile_h,
                sub.w as f32 / tile_w,
                sub.h as f32 / tile_h,
            ];
            Some((src_uv, sub.w, sub.h))
        });
        // U2/U3 in-tile drop-zone highlight: the exact target *pane*'s 60/40
        // zone sub-rect (center = inner box, edge = edge band — the distinct
        // shapes are the non-color cue for swap vs split), the tab tile index
        // it lives in, and the `src_uv` sampling that zone out of the tab tile
        // texture. Resolved before the gpu/overview borrows. `None` (no
        // highlight) whenever the release would `Cancel`: a self-drop, a
        // foreign window group, or the pointer over a divider gap / no pane.
        // Reuses the pure `pane_zone_highlight_rect` the main-view pane drag
        // draws, so the highlight always matches what a release resolves to.
        let drag_zone_highlight: Option<(usize, PaneRectApp, [f32; 4])> =
            active_drag.and_then(|drag| {
                let (dest_tab, target_pane, zone) = self.overview_drop_target_at_last_cursor()?;
                let same_group = {
                    let source_group = self
                        .windows
                        .get(&drag.source.window_id)
                        .map(|state| state.group);
                    let dest_group = self.windows.get(&dest_tab).map(|state| state.group);
                    source_group.is_some() && source_group == dest_group
                };
                if matches!(
                    crate::session_overview::resolve_overview_drop(
                        drag.source.window_id,
                        drag.source.pane_id,
                        Some((dest_tab, target_pane, zone)),
                        same_group,
                    ),
                    crate::session_overview::OverviewDrop::Cancel
                ) {
                    return None;
                }
                let index = source_tab_ids.iter().position(|id| *id == dest_tab)?;
                let tile = *layout.tiles.get(index)?;
                let content =
                    crate::session_overview::tab_tile_content_rect(tile, metrics.title_bar_h);
                let state = self.windows.get(&dest_tab)?;
                let pane_rects =
                    crate::session_overview::tab_tile_pane_rects(content, &state.split_tree);
                let (_, pane_rect) = pane_rects.iter().find(|(pane, _)| *pane == target_pane)?;
                let edge = match zone {
                    PaneZone::Center => None,
                    PaneZone::Edge(direction) => Some(direction),
                };
                let zone_rect =
                    crate::app::pane_drag_render::pane_zone_highlight_rect(*pane_rect, edge);
                let tile_w = tile.w.max(1) as f32;
                let tile_h = tile.h.max(1) as f32;
                let src_uv = [
                    (zone_rect.x.saturating_sub(tile.x)) as f32 / tile_w,
                    (zone_rect.y.saturating_sub(tile.y)) as f32 / tile_h,
                    zone_rect.w as f32 / tile_w,
                    zone_rect.h as f32 / tile_h,
                ];
                Some((index, zone_rect, src_uv))
            });

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
                configure_wgpu_surface(
                    &host_state.surface,
                    &gpu.device,
                    &host_state.surface_config,
                    host_state.occluded,
                );
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
            // `chrome.view` is the one view cached alongside its texture
            // (`OverviewChromeTexture`) — reusing it (rather than calling
            // `create_view` again here) keeps its identity stable across
            // cache-hit frames, which is what lets `CardPipeline`'s
            // per-view bind-group pool skip re-creating GPU resources.
            let mut placements = Vec::new();
            if let Some(chrome) = search_texture.as_ref() {
                placements.push(CardTexturePlacement {
                    texture_view: &chrome.view,
                    x: chrome.rect.x,
                    y: chrome.rect.y,
                    w: chrome.rect.w,
                    h: chrome.rect.h,
                    selected: false,
                });
            }
            if let Some(chrome) = hint_texture.as_ref() {
                placements.push(CardTexturePlacement {
                    texture_view: &chrome.view,
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
                let (attention, emphasized) = attention_tiles
                    .get(index)
                    .copied()
                    .unwrap_or((false, false));
                if !overview_attention_ring_visible(attention, index, selected, hovered) {
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
                    &overview_attention_card_style(metrics, emphasized),
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

            // U2/U3 in-tile drop-zone highlight: a ring around the target
            // pane's resolved 60/40 zone sub-rect (center = inner box, edge =
            // edge band), sampled out of the tab tile texture via `src_uv` so
            // the ring frames exactly the region a release would act on. Only a
            // *valid* drop (non-Cancel) lights up (see `drag_zone_highlight`).
            if let Some((index, zone_rect, src_uv)) = drag_zone_highlight
                && let Some(tile_view) = thumbnails.tile_texture_view(index)
            {
                let drop_style = CardStyle {
                    focus_width: crate::chrome::RING_HOVER * metrics.scale(),
                    focus_glow_width: 0.0,
                    ..overview_card_style(metrics)
                };
                chrome_card.pipeline.overlay_texture_cards_clipped(
                    &gpu.device,
                    &gpu.queue,
                    &view,
                    surface_size,
                    &drop_style,
                    &[CardTexturePlacement {
                        texture_view: &tile_view,
                        x: zone_rect.x,
                        y: zone_rect.y,
                        w: zone_rect.w,
                        h: zone_rect.h,
                        selected: true,
                    }],
                    src_uv,
                    1.0,
                );
            }

            // U4 floating chip: the dragged *pane*'s sub-rect of its tab tile
            // texture (via `src_uv`), shrunk to half that sub-rect's size and
            // centered on the cursor at 70% opacity — reuses the already-
            // composited tile texture, allocates nothing per frame. Drawn last
            // so it rides above every tile and ring.
            if let (Some(drag), Some(index), Some((src_uv, sub_w, sub_h))) =
                (active_drag, drag_source_index, drag_chip)
                && let Some(tile_view) = thumbnails.tile_texture_view(index)
            {
                let chip_w = (sub_w / 2).max(1);
                let chip_h = (sub_h / 2).max(1);
                let chip_x = drag.current_point.x.saturating_sub(chip_w / 2);
                let chip_y = drag.current_point.y.saturating_sub(chip_h / 2);
                chrome_card.pipeline.overlay_texture_cards_clipped(
                    &gpu.device,
                    &gpu.queue,
                    &view,
                    surface_size,
                    &overview_card_style(metrics),
                    &[CardTexturePlacement {
                        texture_view: &tile_view,
                        x: chip_x,
                        y: chip_y,
                        w: chip_w,
                        h: chip_h,
                        selected: false,
                    }],
                    src_uv,
                    0.7,
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
        tab_ids: &[WindowId],
        now: Instant,
    ) {
        for window_id in tab_ids {
            let tile = self.overview_tiles.entry(*window_id).or_default();
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

        // v3 paging: every downstream consumer below works off the current
        // page's tile slice (≤ OVERVIEW_GRID_CAP, always live — no
        // placeholder rows), not the full unpaged source order. Tiles on
        // other pages are simply not candidates for GPU work this frame.
        let page_view = self.overview_page_view();
        let Some(layout) = self.overview_layout(&page_view.slice) else {
            return;
        };
        // REQ-OV-14: keep the selection in range as source panes come and go.
        if let Some(overview) = self.overview_window.as_mut() {
            overview.selected = overview
                .selected
                .min(page_view.slice.len().saturating_sub(1));
        }
        let now = Instant::now();
        let due_tile_ids = self.due_overview_tile_ids(&page_view.slice, now);

        self.ensure_overview_thumbnails(&layout);
        self.render_due_overview_tiles(&due_tile_ids, &page_view.slice);
        self.render_due_overview_title_bands(&due_tile_ids, &page_view.slice, &layout);
        self.render_overview_placeholder_labels(&page_view.slice, &layout);
        self.present_overview_frame(
            &layout,
            &page_view.slice,
            page_view.page,
            page_view.page_count,
        );

        self.finish_overview_tile_renders(&due_tile_ids, now);

        // OVERVIEW_MAX_RENDER_TILES_PER_FRAME caps how many tiles one frame
        // regenerates, and idle tabs produce no pty output to trigger the
        // next frame — so a dirty backlog can survive this frame for two
        // different reasons, and only one of them justifies re-requesting a
        // redraw right away (Fix A): a due-but-capped tile (immediate), vs.
        // a tile that is merely inside its 10Hz throttle window (schedule
        // one delayed wake-up via `tick_overview_backlog` instead of
        // spinning `present_overview_frame` until it's due). Scoped to the
        // current page's slice, same as the due-selection above — a dirty
        // tile on another page doesn't justify waking this page's frame.
        let candidates = self.overview_tile_candidates(&page_view.slice);
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

/// Hit/miss rule for the search/hint pill cache
/// (`render_overview_search_texture` / `render_overview_hint_texture`): the
/// cached value is reusable only if its key matches the current call's
/// exactly. Generic over the cached value type so it's unit-testable without
/// a `wgpu::TextureView`, which needs a live `Device` to construct.
fn overview_pill_cache_hit<'a, V>(
    cached: Option<&'a (OverviewPillKey, V)>,
    key: &OverviewPillKey,
) -> Option<&'a V> {
    let (cached_key, value) = cached?;
    (cached_key == key).then_some(value)
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

    fn pill_key(query: &str, live_tile_count: usize) -> OverviewPillKey {
        OverviewPillKey {
            query: query.to_string(),
            live_tile_count,
            page: 0,
            rect: PaneRectApp::new(0, 0, 200, 32),
        }
    }

    #[test]
    fn pill_cache_hits_when_key_is_unchanged() {
        let cached = Some((pill_key("noa", 3), "pill-texture"));
        let hit = overview_pill_cache_hit(cached.as_ref(), &pill_key("noa", 3));
        assert_eq!(hit, Some(&"pill-texture"));
    }

    #[test]
    fn pill_cache_misses_when_query_changes() {
        let cached = Some((pill_key("noa", 3), "pill-texture"));
        let hit = overview_pill_cache_hit(cached.as_ref(), &pill_key("noab", 3));
        assert_eq!(hit, None);
    }

    #[test]
    fn pill_cache_misses_when_live_tile_count_changes() {
        let cached = Some((pill_key("noa", 3), "pill-texture"));
        let hit = overview_pill_cache_hit(cached.as_ref(), &pill_key("noa", 4));
        assert_eq!(hit, None);
    }

    #[test]
    fn pill_cache_misses_when_rect_changes() {
        let cached = Some((pill_key("noa", 3), "pill-texture"));
        let resized = OverviewPillKey {
            rect: PaneRectApp::new(0, 0, 260, 32),
            ..pill_key("noa", 3)
        };
        let hit = overview_pill_cache_hit(cached.as_ref(), &resized);
        assert_eq!(hit, None);
    }

    // C2 (v3 paging): a page flip must invalidate the cached pill texture —
    // the hint pill's "Page p/N" segment changes even when `query` /
    // `live_tile_count` / `rect` are all unchanged.
    #[test]
    fn pill_cache_misses_when_page_changes() {
        let cached = Some((pill_key("noa", 3), "pill-texture"));
        let flipped = OverviewPillKey {
            page: 1,
            ..pill_key("noa", 3)
        };
        let hit = overview_pill_cache_hit(cached.as_ref(), &flipped);
        assert_eq!(hit, None);
    }

    #[test]
    fn pill_cache_misses_when_nothing_cached_yet() {
        let hit = overview_pill_cache_hit(None::<&(OverviewPillKey, &str)>, &pill_key("noa", 3));
        assert_eq!(hit, None);
    }
}
