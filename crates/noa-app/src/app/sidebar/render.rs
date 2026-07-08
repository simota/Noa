use std::fmt::Write as _;

use super::*;

/// Rasterize one synthetic sidebar grid (background + positioned text/dots)
/// with the reused `Renderer` into `view`. `base_bg` fills the empty cells and
/// the clear color so a card texture reads as its own surface. `bg_opacity`
/// scales that clear color's alpha (1.0 keeps it fully opaque); the card
/// composite shader now passes the sampled texture's alpha through
/// (`card.wgsl`), so this is what makes the caller's backdrop translucent.
#[allow(clippy::too_many_arguments)]
fn rasterize_runs(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    font: &mut FontGrid,
    theme: &Theme,
    view: &wgpu::TextureView,
    size: PixelSize,
    grid: GridSize,
    base_bg: Rgb,
    bg_opacity: f32,
    runs: &[SidebarTextRun],
) {
    let mut term = Terminal::new(grid);
    term.set_base_colors(chrome().fg, base_bg, chrome().fg, theme.palette);
    let mut stream = Stream::new();
    // Autowrap off so a long cwd/preview clips at the right margin instead of
    // wrapping to the next row and shifting every run below it.
    stream.feed(b"\x1b[?7l", &mut term);
    let mut feed = String::new();
    for run in runs {
        feed.clear();
        // CUP is 1-based; position, optional bold, truecolor fg (+bg), write, reset.
        let _ = write!(feed, "\x1b[{};{}H", run.row + 1, run.col + 1);
        if run.bold {
            let _ = write!(feed, "\x1b[1m");
        }
        let _ = write!(feed, "\x1b[38;2;{};{};{}m", run.fg.r, run.fg.g, run.fg.b);
        if let Some(bg) = run.bg {
            let _ = write!(feed, "\x1b[48;2;{};{};{}m", bg.r, bg.g, bg.b);
        }
        let _ = write!(feed, "{}\x1b[0m", run.text);
        stream.feed(feed.as_bytes(), &mut term);
    }
    let mut snapshot = FrameSnapshot::from_terminal(&mut term);
    snapshot.cursor.visible = false;

    renderer.resize(size);
    renderer.rebuild_cells(&snapshot, font, theme);
    let mut clear_color = rgb_to_rgba(base_bg);
    clear_color[3] *= bg_opacity.clamp(0.0, 1.0);
    renderer.set_clear_color(clear_color);
    renderer.sync_atlas(device, queue, font);
    renderer.draw(device, queue, view);
}

/// Ensure `slot` holds a scratch render texture of exactly `size`/`format`,
/// reallocating only when either changes (reused frame-to-frame — F2).
/// Returns whether it actually (re)built the texture, so `ChromeTextures`
/// call sites can feed [`ChromeTextures::record_rebuild`] (NFR-2/AC-18).
#[must_use]
pub(super) fn ensure_scratch(
    slot: &mut Option<(PixelSize, wgpu::Texture, wgpu::TextureView)>,
    device: &wgpu::Device,
    size: PixelSize,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> bool {
    let size = PixelSize {
        w: size.w.max(1),
        h: size.h.max(1),
    };
    let rebuilt = slot
        .as_ref()
        .is_none_or(|(s, t, _)| *s != size || t.format() != format);
    if rebuilt {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.w,
                height: size.h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        *slot = Some((size, texture, view));
    }
    rebuilt
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SidebarCardFrame {
    Resting,
    Selected,
    Attention,
}

fn sidebar_card_frame(selected: bool, attention: bool) -> SidebarCardFrame {
    if selected {
        SidebarCardFrame::Selected
    } else if attention {
        SidebarCardFrame::Attention
    } else {
        SidebarCardFrame::Resting
    }
}

/// Rasterize the sidebar and composite it onto `view` at the window's left
/// inset via the reused rounded-card pipeline: a flat backdrop matching the
/// terminal theme's background (so the band reads as one surface with the
/// panes), then each fully-visible card as a rounded card with a subtle
/// border and a focus ring on the selected one, then the optional `…` menu
/// popup above them all. Runs inline in `redraw` with the already-borrowed
/// `gpu`, so the model must be prebuilt (no `self` here).
pub(in crate::app) fn draw_sidebar_band(
    gpu: &mut GpuState,
    surface_format: wgpu::TextureFormat,
    padding: GridPadding,
    view: &wgpu::TextureView,
    surface_size: PixelSize,
    model: &SidebarDrawModel,
) {
    // Lazily (re)build the reused band renderer + card pipeline for this format.
    if gpu
        .chrome_textures
        .sidebar_renderer
        .as_ref()
        .is_none_or(|renderer| renderer.target_format() != surface_format)
    {
        gpu.chrome_textures.sidebar_renderer = Renderer::new(
            &gpu.device,
            &gpu.queue,
            surface_format,
            &mut gpu.sidebar_font,
            padding,
        )
        .ok();
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
    if gpu
        .chrome_textures
        .sidebar_card
        .as_ref()
        .is_none_or(|card| card.format != surface_format)
    {
        gpu.chrome_textures.sidebar_card = Some(OverviewChromeCardPipeline {
            format: surface_format,
            // Alpha-replace so card/menu/divider composites settle to a uniform
            // background-opacity alpha instead of accumulating toward opaque.
            pipeline: CardPipeline::new(&gpu.device, surface_format, CardPipeline::ALPHA_REPLACE),
        });
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
    if gpu
        .chrome_textures
        .sidebar_band_card
        .as_ref()
        .is_none_or(|card| card.format != surface_format)
    {
        gpu.chrome_textures.sidebar_band_card = Some(OverviewChromeCardPipeline {
            format: surface_format,
            // The band backdrop is transparent outside its text runs; plain
            // alpha blending leaves the pane pass's clear color + background
            // image untouched there, so the band background is pixel-identical
            // to the panes'.
            pipeline: CardPipeline::new(
                &gpu.device,
                surface_format,
                wgpu::BlendState::ALPHA_BLENDING,
            ),
        });
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
    let band_size = PixelSize {
        w: model.inset.max(1),
        h: model.height.max(1),
    };
    if ensure_scratch(
        &mut gpu.chrome_textures.sidebar_band,
        &gpu.device,
        band_size,
        surface_format,
        "noa-sidebar-band",
    ) {
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
    if !model.cards.is_empty()
        && ensure_scratch(
            &mut gpu.chrome_textures.sidebar_card_tex,
            &gpu.device,
            PixelSize {
                w: model.card_w,
                h: model.card_h,
            },
            surface_format,
            "noa-sidebar-card",
        )
    {
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }
    if let Some(menu) = &model.menu
        && ensure_scratch(
            &mut gpu.chrome_textures.sidebar_menu_tex,
            &gpu.device,
            PixelSize {
                w: menu.rect.w,
                h: menu.rect.h,
            },
            surface_format,
            "noa-sidebar-menu",
        )
    {
        #[cfg(debug_assertions)]
        gpu.chrome_textures.record_rebuild();
    }

    if gpu.chrome_textures.sidebar_renderer.is_none()
        || gpu.chrome_textures.sidebar_card.is_none()
        || gpu.chrome_textures.sidebar_band_card.is_none()
        || gpu.chrome_textures.sidebar_band.is_none()
    {
        return;
    }

    // 1) Band text runs over a fully transparent base → band texture,
    // alpha-blended over the inset with no rounding, so the pane pass's clear
    // color + background image underneath stay untouched and the band's
    // background is pixel-identical to the panes'. The placement is drawn
    // `selected` with a black focus color and zero focus stroke, which turns
    // the card shader's outer glow into a soft shadow the band casts onto the
    // panes — the seam's depth cue (its crisp line is the hairline below).
    {
        let band_view = &gpu.chrome_textures.sidebar_band.as_ref().unwrap().2;
        rasterize_runs(
            gpu.chrome_textures.sidebar_renderer.as_mut().unwrap(),
            &gpu.device,
            &gpu.queue,
            &mut gpu.sidebar_font,
            active_theme(&gpu.theme, &gpu.preview_theme),
            band_view,
            band_size,
            model.grid,
            active_theme(&gpu.theme, &gpu.preview_theme).default_bg,
            0.0,
            &model.runs,
        );
    }
    let flat_style = CardStyle {
        background: rgb_to_rgba(active_theme(&gpu.theme, &gpu.preview_theme).default_bg),
        border_color: [0.0; 4],
        focus_color: [0.0, 0.0, 0.0, 1.0],
        corner_radius: 0.0,
        border_width: 0.0,
        focus_width: 0.0,
        focus_glow_width: SEAM_SHADOW_WIDTH * model.scale,
    };
    gpu.chrome_textures
        .sidebar_band_card
        .as_ref()
        .unwrap()
        .pipeline
        .overlay_texture_cards(
            &gpu.device,
            &gpu.queue,
            view,
            surface_size,
            &flat_style,
            &[CardTexturePlacement {
                texture_view: &gpu.chrome_textures.sidebar_band.as_ref().unwrap().2,
                x: 0,
                y: 0,
                w: model.inset,
                h: model.height,
                selected: true,
            }],
        );

    // 1b) Hairline divider over the band's rightmost pixel(s): a solid
    // `chrome().divider` strip that gives the seam a crisp edge against the
    // pane background (the terminal keeps its own theme, so the two surfaces
    // otherwise meet as unrelated colors).
    let hairline_w = (SEAM_HAIRLINE_WIDTH * model.scale).round().max(1.0) as u32;
    if SEAM_HAIRLINE_WIDTH > 0.0 && model.inset > hairline_w {
        if ensure_scratch(
            &mut gpu.chrome_textures.sidebar_divider_tex,
            &gpu.device,
            PixelSize {
                w: hairline_w,
                h: model.height,
            },
            surface_format,
            "noa-sidebar-divider",
        ) {
            #[cfg(debug_assertions)]
            gpu.chrome_textures.record_rebuild();
        }
        if let Some((_, _, divider_view)) = gpu.chrome_textures.sidebar_divider_tex.as_ref() {
            rasterize_runs(
                gpu.chrome_textures.sidebar_renderer.as_mut().unwrap(),
                &gpu.device,
                &gpu.queue,
                &mut gpu.sidebar_font,
                active_theme(&gpu.theme, &gpu.preview_theme),
                divider_view,
                PixelSize {
                    w: hairline_w,
                    h: model.height,
                },
                GridSize { cols: 1, rows: 1 },
                chrome().divider,
                1.0,
                &[],
            );
            let divider_style = CardStyle {
                background: rgb_to_rgba(chrome().divider),
                border_color: [0.0; 4],
                focus_color: [0.0; 4],
                corner_radius: 0.0,
                border_width: 0.0,
                focus_width: 0.0,
                focus_glow_width: 0.0,
            };
            gpu.chrome_textures
                .sidebar_card
                .as_ref()
                .unwrap()
                .pipeline
                .overlay_texture_cards(
                    &gpu.device,
                    &gpu.queue,
                    view,
                    surface_size,
                    &divider_style,
                    &[CardTexturePlacement {
                        texture_view: divider_view,
                        x: model.inset - hairline_w,
                        y: 0,
                        w: hairline_w,
                        h: model.height,
                        selected: false,
                    }],
                );
        }
    }

    // 1c) Toolbar `+` button: a borderless geometric `+` glyph — two centered
    // bars, drawn as solid rounded rects (pixel-placed rather than a font glyph,
    // which the coarse sidebar cell grid can't center in a tile this small). No
    // persistent frame; hover just lays a subtle borderless fill behind the `+`
    // and brightens the bars.
    if model.new_button.w > 0 && model.new_button.h > 0 {
        let btn = model.new_button;
        let btn_size = PixelSize {
            w: btn.w.max(1),
            h: btn.h.max(1),
        };
        if ensure_scratch(
            &mut gpu.chrome_textures.sidebar_button_tex,
            &gpu.device,
            btn_size,
            surface_format,
            "noa-sidebar-button",
        ) {
            #[cfg(debug_assertions)]
            gpu.chrome_textures.record_rebuild();
        }
        let glyph = if model.new_button_hover {
            chrome().fg
        } else {
            chrome().dim_fg
        };

        // Hover only: a borderless rounded fill behind the `+` (no frame at
        // rest). Rendered into the reused button scratch, composited over the
        // band.
        if model.new_button_hover {
            let button_view = &gpu.chrome_textures.sidebar_button_tex.as_ref().unwrap().2;
            rasterize_runs(
                gpu.chrome_textures.sidebar_renderer.as_mut().unwrap(),
                &gpu.device,
                &gpu.queue,
                &mut gpu.sidebar_font,
                active_theme(&gpu.theme, &gpu.preview_theme),
                button_view,
                btn_size,
                GridSize { cols: 1, rows: 1 },
                chrome().card,
                model.background_opacity,
                &[],
            );
            let button_style = CardStyle {
                background: rgb_to_rgba(chrome().card),
                border_color: [0.0; 4],
                focus_color: [0.0; 4],
                corner_radius: TOOLBAR_BUTTON_RADIUS * model.scale,
                border_width: 0.0,
                focus_width: 0.0,
                focus_glow_width: 0.0,
            };
            gpu.chrome_textures
                .sidebar_card
                .as_ref()
                .unwrap()
                .pipeline
                .overlay_texture_cards(
                    &gpu.device,
                    &gpu.queue,
                    view,
                    surface_size,
                    &button_style,
                    &[CardTexturePlacement {
                        texture_view: &gpu.chrome_textures.sidebar_button_tex.as_ref().unwrap().2,
                        x: btn.x,
                        y: btn.y,
                        w: btn.w,
                        h: btn.h,
                        selected: false,
                    }],
                );
        }

        // The `+` glyph: refill the same scratch with the glyph color (the hover
        // fill composite above, if any, already submitted, so the reuse is safe)
        // and composite two thin rounded bars centered on the tile.
        let arm = (TOOLBAR_PLUS_ARM * model.scale).round().max(1.0) as u32;
        let thick = (TOOLBAR_PLUS_THICKNESS * model.scale).round().max(1.0) as u32;
        let cx = btn.x + btn.w / 2;
        let cy = btn.y + btn.h / 2;
        let hbar = SidebarRect::new(
            cx.saturating_sub(arm / 2),
            cy.saturating_sub(thick / 2),
            arm,
            thick,
        );
        let vbar = SidebarRect::new(
            cx.saturating_sub(thick / 2),
            cy.saturating_sub(arm / 2),
            thick,
            arm,
        );
        let glyph_view = &gpu.chrome_textures.sidebar_button_tex.as_ref().unwrap().2;
        rasterize_runs(
            gpu.chrome_textures.sidebar_renderer.as_mut().unwrap(),
            &gpu.device,
            &gpu.queue,
            &mut gpu.sidebar_font,
            active_theme(&gpu.theme, &gpu.preview_theme),
            glyph_view,
            btn_size,
            GridSize { cols: 1, rows: 1 },
            glyph,
            model.background_opacity,
            &[],
        );
        let bar_style = CardStyle {
            background: rgb_to_rgba(glyph),
            border_color: [0.0; 4],
            focus_color: [0.0; 4],
            corner_radius: (thick as f32 / 2.0).min(2.0 * model.scale),
            border_width: 0.0,
            focus_width: 0.0,
            focus_glow_width: 0.0,
        };
        gpu.chrome_textures
            .sidebar_card
            .as_ref()
            .unwrap()
            .pipeline
            .overlay_texture_cards(
                &gpu.device,
                &gpu.queue,
                view,
                surface_size,
                &bar_style,
                &[
                    CardTexturePlacement {
                        texture_view: &gpu.chrome_textures.sidebar_button_tex.as_ref().unwrap().2,
                        x: hbar.x,
                        y: hbar.y,
                        w: hbar.w,
                        h: hbar.h,
                        selected: false,
                    },
                    CardTexturePlacement {
                        texture_view: &gpu.chrome_textures.sidebar_button_tex.as_ref().unwrap().2,
                        x: vbar.x,
                        y: vbar.y,
                        w: vbar.w,
                        h: vbar.h,
                        selected: false,
                    },
                ],
            );
    }

    // 2) Each fully-visible card as a rounded card. One reused scratch texture
    // serves every card in turn (render → composite), so submits serialize the
    // reuse safely.
    let card_style = CardStyle {
        background: rgb_to_rgba(chrome().card),
        border_color: rgb_to_rgba(chrome().border),
        focus_color: rgb_to_rgba(chrome().accent),
        corner_radius: crate::chrome::RADIUS_LG * model.scale,
        border_width: 1.0 * model.scale,
        focus_width: crate::chrome::RING_SELECTED * model.scale,
        focus_glow_width: 0.0,
    };
    // A non-focused card with a pending interaction request swaps the blue
    // focus accent for a red ring (FR-16), drawn through the selected branch.
    // Sidebar cards do not use an outer selected glow, so attention follows
    // that treatment and leaves the red dot/label to carry the extra urgency.
    let attention_style = CardStyle {
        focus_color: rgb_to_rgba(chrome().dot_red),
        focus_width: crate::chrome::RING_ATTENTION * model.scale,
        focus_glow_width: 0.0,
        ..card_style
    };
    for card_draw in &model.cards {
        let Some((_, _, card_view)) = gpu.chrome_textures.sidebar_card_tex.as_ref() else {
            break;
        };
        rasterize_runs(
            gpu.chrome_textures.sidebar_renderer.as_mut().unwrap(),
            &gpu.device,
            &gpu.queue,
            &mut gpu.sidebar_font,
            active_theme(&gpu.theme, &gpu.preview_theme),
            card_view,
            PixelSize {
                w: model.card_w,
                h: model.card_h,
            },
            card_draw.grid,
            card_draw.bg,
            model.background_opacity,
            &card_draw.runs,
        );
        let (style, selected) = match sidebar_card_frame(card_draw.selected, card_draw.attention) {
            SidebarCardFrame::Resting => (&card_style, false),
            SidebarCardFrame::Selected => (&card_style, true),
            SidebarCardFrame::Attention => (&attention_style, true),
        };
        gpu.chrome_textures
            .sidebar_card
            .as_ref()
            .unwrap()
            .pipeline
            .overlay_texture_cards(
                &gpu.device,
                &gpu.queue,
                view,
                surface_size,
                style,
                &[CardTexturePlacement {
                    texture_view: &gpu.chrome_textures.sidebar_card_tex.as_ref().unwrap().2,
                    x: card_draw.rect.x,
                    y: card_draw.rect.y,
                    w: card_draw.rect.w,
                    h: card_draw.rect.h,
                    selected,
                }],
            );

        // Status accent bar (busy / attention / bell) along the card's left
        // edge: a thin solid capsule composited over the card border, inset
        // past the corner radius so it never pokes outside the rounding. One
        // reused scratch serves every bar in turn (same pattern as the card
        // texture), refilled per card because the color varies.
        if let Some(accent) = card_draw.accent {
            let bar_w = (2.0 * model.scale).round().max(1.0) as u32;
            let inset_y = (crate::chrome::RADIUS_LG * model.scale).round() as u32;
            let bar_h = card_draw.rect.h.saturating_sub(2 * inset_y);
            if bar_h > 0 {
                if ensure_scratch(
                    &mut gpu.chrome_textures.sidebar_accent_tex,
                    &gpu.device,
                    PixelSize { w: bar_w, h: bar_h },
                    surface_format,
                    "noa-sidebar-accent",
                ) {
                    #[cfg(debug_assertions)]
                    gpu.chrome_textures.record_rebuild();
                }
                if let Some((_, _, accent_view)) = gpu.chrome_textures.sidebar_accent_tex.as_ref()
                {
                    rasterize_runs(
                        gpu.chrome_textures.sidebar_renderer.as_mut().unwrap(),
                        &gpu.device,
                        &gpu.queue,
                        &mut gpu.sidebar_font,
                        active_theme(&gpu.theme, &gpu.preview_theme),
                        accent_view,
                        PixelSize { w: bar_w, h: bar_h },
                        GridSize { cols: 1, rows: 1 },
                        accent,
                        1.0,
                        &[],
                    );
                    let accent_style = CardStyle {
                        background: rgb_to_rgba(accent),
                        border_color: [0.0; 4],
                        focus_color: [0.0; 4],
                        corner_radius: bar_w as f32 / 2.0,
                        border_width: 0.0,
                        focus_width: 0.0,
                        focus_glow_width: 0.0,
                    };
                    gpu.chrome_textures
                        .sidebar_card
                        .as_ref()
                        .unwrap()
                        .pipeline
                        .overlay_texture_cards(
                            &gpu.device,
                            &gpu.queue,
                            view,
                            surface_size,
                            &accent_style,
                            &[CardTexturePlacement {
                                texture_view: accent_view,
                                x: card_draw.rect.x,
                                y: card_draw.rect.y + inset_y,
                                w: bar_w,
                                h: bar_h,
                                selected: false,
                            }],
                        );
                }
            }
        }
    }

    // 2b) Drag-reorder feedback: the accent drop-indicator line at the insertion
    // gap, then the floating dragged card composited above every static card.
    if let Some(line) = &model.drop_indicator {
        if ensure_scratch(
            &mut gpu.chrome_textures.sidebar_drop_tex,
            &gpu.device,
            PixelSize {
                w: line.w.max(1),
                h: line.h.max(1),
            },
            surface_format,
            "noa-sidebar-drop",
        ) {
            #[cfg(debug_assertions)]
            gpu.chrome_textures.record_rebuild();
        }
        if let Some((_, _, drop_view)) = gpu.chrome_textures.sidebar_drop_tex.as_ref() {
            rasterize_runs(
                gpu.chrome_textures.sidebar_renderer.as_mut().unwrap(),
                &gpu.device,
                &gpu.queue,
                &mut gpu.sidebar_font,
                active_theme(&gpu.theme, &gpu.preview_theme),
                drop_view,
                PixelSize {
                    w: line.w.max(1),
                    h: line.h.max(1),
                },
                GridSize { cols: 1, rows: 1 },
                chrome().accent,
                1.0,
                &[],
            );
            let drop_style = CardStyle {
                background: rgb_to_rgba(chrome().accent),
                border_color: [0.0; 4],
                focus_color: [0.0; 4],
                corner_radius: (line.h as f32 / 2.0).min(3.0 * model.scale),
                border_width: 0.0,
                focus_width: 0.0,
                focus_glow_width: 0.0,
            };
            gpu.chrome_textures
                .sidebar_card
                .as_ref()
                .unwrap()
                .pipeline
                .overlay_texture_cards(
                    &gpu.device,
                    &gpu.queue,
                    view,
                    surface_size,
                    &drop_style,
                    &[CardTexturePlacement {
                        texture_view: drop_view,
                        x: line.x,
                        y: line.y,
                        w: line.w,
                        h: line.h,
                        selected: false,
                    }],
                );
        }
    }
    if let Some(drag) = &model.dragging
        && let Some((_, _, card_view)) = gpu.chrome_textures.sidebar_card_tex.as_ref()
    {
        rasterize_runs(
            gpu.chrome_textures.sidebar_renderer.as_mut().unwrap(),
            &gpu.device,
            &gpu.queue,
            &mut gpu.sidebar_font,
            active_theme(&gpu.theme, &gpu.preview_theme),
            card_view,
            PixelSize {
                w: model.card_w,
                h: model.card_h,
            },
            drag.grid,
            drag.bg,
            model.background_opacity,
            &drag.runs,
        );
        gpu.chrome_textures
            .sidebar_card
            .as_ref()
            .unwrap()
            .pipeline
            .overlay_texture_cards(
                &gpu.device,
                &gpu.queue,
                view,
                surface_size,
                &card_style,
                &[CardTexturePlacement {
                    texture_view: &gpu.chrome_textures.sidebar_card_tex.as_ref().unwrap().2,
                    x: drag.rect.x,
                    y: drag.rect.y,
                    w: drag.rect.w,
                    h: drag.rect.h,
                    selected: true,
                }],
            );
    }

    // 3) The `…` menu popup, composited above the cards.
    if let Some(menu) = &model.menu
        && let Some((_, _, menu_view)) = gpu.chrome_textures.sidebar_menu_tex.as_ref()
    {
        rasterize_runs(
            gpu.chrome_textures.sidebar_renderer.as_mut().unwrap(),
            &gpu.device,
            &gpu.queue,
            &mut gpu.sidebar_font,
            active_theme(&gpu.theme, &gpu.preview_theme),
            menu_view,
            PixelSize {
                w: menu.rect.w,
                h: menu.rect.h,
            },
            menu.grid,
            chrome().pill,
            1.0,
            &menu.runs,
        );
        let menu_style = CardStyle {
            background: rgb_to_rgba(chrome().pill),
            border_color: rgb_to_rgba(chrome().border),
            focus_color: [0.0; 4],
            corner_radius: crate::chrome::RADIUS_SM * model.scale,
            border_width: 1.0 * model.scale,
            focus_width: 0.0,
            focus_glow_width: 0.0,
        };
        gpu.chrome_textures
            .sidebar_card
            .as_ref()
            .unwrap()
            .pipeline
            .overlay_texture_cards(
                &gpu.device,
                &gpu.queue,
                view,
                surface_size,
                &menu_style,
                &[CardTexturePlacement {
                    texture_view: &gpu.chrome_textures.sidebar_menu_tex.as_ref().unwrap().2,
                    x: menu.rect.x,
                    y: menu.rect.y,
                    w: menu.rect.w,
                    h: menu.rect.h,
                    selected: false,
                }],
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_frame_does_not_override_selected_sidebar_card() {
        assert_eq!(sidebar_card_frame(false, false), SidebarCardFrame::Resting);
        assert_eq!(sidebar_card_frame(false, true), SidebarCardFrame::Attention);
        assert_eq!(sidebar_card_frame(true, false), SidebarCardFrame::Selected);
        assert_eq!(sidebar_card_frame(true, true), SidebarCardFrame::Selected);
    }
}
