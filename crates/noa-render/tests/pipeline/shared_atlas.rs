use super::shared::*;
use noa_core::{DEFAULT_GRID_PADDING, PixelSize};
use noa_font::FontGrid;
use noa_render::{GlyphAtlasCache, PipelineCache, Renderer, Theme};

#[test]
fn glyph_atlas_cache_is_keyed_by_target_format() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available - skipping GPU shared-atlas format-key test");
        return;
    };
    let mut cache = GlyphAtlasCache::default();
    let font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");

    let non_srgb = cache.get(&device, &queue, wgpu::TextureFormat::Bgra8Unorm, &font);
    let srgb = cache.get(&device, &queue, wgpu::TextureFormat::Bgra8UnormSrgb, &font);

    assert_eq!(non_srgb.format(), wgpu::TextureFormat::Bgra8Unorm);
    assert_eq!(srgb.format(), wgpu::TextureFormat::Bgra8UnormSrgb);
}

#[test]
fn two_renderers_sharing_glyph_atlas_draw_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available - skipping GPU shared-atlas draw test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut pipeline_cache = PipelineCache::default();
    let pipelines = pipeline_cache.get(&device, format);
    let mut atlas_cache = GlyphAtlasCache::default();
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let atlases = atlas_cache.get(&device, &queue, format, &font);

    let mut first = Renderer::with_pipelines(
        &device,
        &queue,
        &pipelines,
        &atlases,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build first renderer");
    let mut second = Renderer::with_pipelines(
        &device,
        &queue,
        &pipelines,
        &atlases,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build second renderer");
    first.resize(PixelSize { w: 96, h: 40 });
    second.resize(PixelSize { w: 96, h: 40 });

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let (_first_target, first_view) = render_target(&device, 96, 40);
    rebuild_text_frame(&mut first, &mut font, &device, &queue, "first tab");
    first.draw(&device, &queue, &first_view);
    let (_second_target, second_view) = render_target(&device, 96, 40);
    rebuild_text_frame(&mut second, &mut font, &device, &queue, "second tab");
    second.draw(&device, &queue, &second_view);
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error drawing two renderers with shared atlas: {err:?}"
    );
}

#[test]
fn shared_glyph_atlas_reallocation_refreshes_both_renderers_before_draw() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available - skipping GPU shared-atlas growth test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut pipeline_cache = PipelineCache::default();
    let pipelines = pipeline_cache.get(&device, format);
    let mut atlas_cache = GlyphAtlasCache::default();
    let mut font = FontGrid::new(220.0, noa_font::FontConfig::default())
        .expect("load a system monospace font");
    let atlases = atlas_cache.get(&device, &queue, format, &font);

    let mut first = Renderer::with_pipelines(
        &device,
        &queue,
        &pipelines,
        &atlases,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build first renderer");
    let mut second = Renderer::with_pipelines(
        &device,
        &queue,
        &pipelines,
        &atlases,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build second renderer");
    first.resize(PixelSize { w: 512, h: 256 });
    second.resize(PixelSize { w: 512, h: 256 });

    let (_initial_first_target, initial_first_view) = render_target(&device, 512, 256);
    rebuild_text_frame(&mut first, &mut font, &device, &queue, "A");
    first.draw(&device, &queue, &initial_first_view);
    let (_initial_second_target, initial_second_view) = render_target(&device, 512, 256);
    rebuild_text_frame(&mut second, &mut font, &device, &queue, "B");
    second.draw(&device, &queue, &initial_second_view);
    let before_first = first.pane_bind_group_rebuild_counts();
    let before_second = second.pane_bind_group_rebuild_counts();
    let before_size = font.mask_atlas_size();

    let pressure = snapshot_for_text(&large_visible_glyph_string());
    first.rebuild_cells(&pressure, &mut font, &Theme::new());
    if font.mask_atlas_size() == before_size {
        eprintln!(
            "large glyph pressure did not grow the atlas - skipping shared-atlas growth test"
        );
        return;
    }

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    first.sync_atlas(&device, &queue, &mut font);
    let (_first_target, first_view) = render_target(&device, 512, 256);
    first.draw(&device, &queue, &first_view);
    let (_second_target, second_view) = render_target(&device, 512, 256);
    second.draw(&device, &queue, &second_view);
    let err = pollster::block_on(device.pop_error_scope());
    let after_first = first.pane_bind_group_rebuild_counts();
    let after_second = second.pane_bind_group_rebuild_counts();

    assert!(
        before_first
            .iter()
            .zip(after_first.iter())
            .all(|(before, after)| after > before),
        "syncing renderer must refresh bind groups after shared atlas growth: before={before_first:?} after={after_first:?}"
    );
    assert!(
        before_second
            .iter()
            .zip(after_second.iter())
            .all(|(before, after)| after > before),
        "non-syncing renderer must refresh stale bind groups before draw: before={before_second:?} after={after_second:?}"
    );
    assert!(
        err.is_none(),
        "wgpu validation error after shared atlas growth draw: {err:?}"
    );
}

/// Regression for the reduced initial atlas dimensions: growing the atlas
/// (texture recreate + full re-upload + bind-group refresh) must leave glyph
/// rendering pixel-identical. A known glyph is drawn and read back, then the
/// atlas is forced to grow under heavy glyph pressure, and the SAME glyph is
/// drawn again — its output must be unchanged (growth is transparent to what
/// reaches the screen), and it must not silently render blank. This is the
/// AC1(b) "correct placement/UV after growth" check the no-validation-error
/// growth tests above do not make.
#[test]
fn glyph_renders_identically_after_atlas_growth() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available - skipping post-growth glyph render test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    // A large pixel size so the pressure string overflows the small initial
    // atlas and forces at least one growth.
    let mut font = FontGrid::new(220.0, noa_font::FontConfig::default())
        .expect("load a system monospace font");
    let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
        .expect("build renderer");
    let (w, h) = (512u32, 256u32);
    renderer.resize(PixelSize { w, h });

    // Draw a single known glyph that fits the initial atlas, and read it back.
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "W");
    let (before_target, before_view) = render_target(&device, w, h);
    renderer.draw(&device, &queue, &before_view);
    let before = read_rgba_pixels(&device, &queue, &before_target, w, h);
    assert!(
        non_background_pixel_count(&before) > 0,
        "the reference glyph should render visible ink before any atlas growth"
    );

    // Force the atlas to grow under heavy glyph pressure.
    let size_before = font.mask_atlas_size();
    rebuild_text_frame(
        &mut renderer,
        &mut font,
        &device,
        &queue,
        &large_visible_glyph_string(),
    );
    if font.mask_atlas_size() == size_before {
        eprintln!("large glyph pressure did not grow the atlas - skipping post-growth render test");
        return;
    }

    // Re-draw the same glyph after the grow: same absolute texels, larger
    // texture, refreshed bind groups. The output must be identical.
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "W");
    let (after_target, after_view) = render_target(&device, w, h);
    renderer.draw(&device, &queue, &after_view);
    let err = pollster::block_on(device.pop_error_scope());
    let after = read_rgba_pixels(&device, &queue, &after_target, w, h);

    assert!(
        err.is_none(),
        "wgpu validation error re-drawing a glyph after atlas growth: {err:?}"
    );
    assert!(
        non_background_pixel_count(&after) > 0,
        "the glyph must still render visible ink after the atlas grew (not blank)"
    );
    assert_eq!(
        hash_pixels(&before),
        hash_pixels(&after),
        "atlas growth must be transparent to rendering: a glyph's pixels must be identical \
         before and after the texture is recreated and re-uploaded"
    );
}

/// Count pixels whose RGB differs from the top-left (background) pixel — a
/// cheap "did anything actually draw" oracle for a solid-background frame.
fn non_background_pixel_count(rgba: &[u8]) -> usize {
    let bg = &rgba[0..3];
    rgba
        .chunks_exact(4)
        .filter(|px| px[0] != bg[0] || px[1] != bg[1] || px[2] != bg[2])
        .count()
}

fn large_visible_glyph_string() -> String {
    ('!'..='~')
        .chain('\u{00A1}'..='\u{017F}')
        .chain('\u{0370}'..='\u{03FF}')
        .chain('\u{0400}'..='\u{04FF}')
        .chain('\u{3041}'..='\u{3096}')
        .take(512)
        .collect()
}
