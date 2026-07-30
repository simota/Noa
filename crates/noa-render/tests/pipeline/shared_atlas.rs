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

/// A `FontGrid`'s atlases hold glyphs rasterized for exactly one pixel size,
/// so two sizes must not land in the same texture set even at the same format.
#[test]
fn glyph_atlas_cache_is_keyed_by_pixel_size() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available - skipping GPU atlas ppem-key test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut cache = GlyphAtlasCache::default();
    let small =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let large =
        FontGrid::new(28.0, noa_font::FontConfig::default()).expect("load a system monospace font");

    let a = cache.get(&device, &queue, format, &small);
    let b = cache.get(&device, &queue, format, &large);
    let a_again = cache.get(&device, &queue, format, &small);

    assert_ne!(
        a.id(),
        b.id(),
        "two pixel sizes must get separate atlas sets, or each sync overwrites the other"
    );
    assert_eq!(
        a.id(),
        a_again.id(),
        "the same pixel size must keep sharing one set"
    );
}

/// The corruption this keying exists to prevent, checked end to end on real
/// pixels rather than on cache identity.
///
/// A renderer's per-row instance caches hold *concrete atlas coordinates*. If
/// two font sizes shared one texture set, building the second renderer would
/// upload the second size's atlas over the first's, and the first renderer's
/// next draw would sample the wrong texels at coordinates it still believes.
/// The redraw below is deliberately a bare `draw` — no rebuild, no re-sync —
/// because a rebuild would re-upload the first atlas and hide exactly the
/// window being tested.
#[test]
fn a_second_font_size_does_not_corrupt_the_first_renderers_atlas() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available - skipping cross-size atlas corruption test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut pipeline_cache = PipelineCache::default();
    let pipelines = pipeline_cache.get(&device, format);
    let mut atlas_cache = GlyphAtlasCache::default();
    let (w, h) = (256u32, 128u32);

    let mut small =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let small_atlases = atlas_cache.get(&device, &queue, format, &small);
    let mut small_renderer = Renderer::with_pipelines(
        &device,
        &queue,
        &pipelines,
        &small_atlases,
        &mut small,
        DEFAULT_GRID_PADDING,
    )
    .expect("build small renderer");
    small_renderer.resize(PixelSize { w, h });

    rebuild_text_frame(&mut small_renderer, &mut small, &device, &queue, "W");
    let (first_target, first_view) = render_target(&device, w, h);
    small_renderer.draw(&device, &queue, &first_view);
    let first = read_rgba_pixels(&device, &queue, &first_target, w, h);
    assert!(
        non_background_pixel_count(&first) > 0,
        "the reference glyph must render visible ink"
    );

    // A second window at a different scale factor: its own grid, its own
    // atlas set, its own renderer — and it syncs and draws in between.
    let mut large =
        FontGrid::new(28.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let large_atlases = atlas_cache.get(&device, &queue, format, &large);
    let mut large_renderer = Renderer::with_pipelines(
        &device,
        &queue,
        &pipelines,
        &large_atlases,
        &mut large,
        DEFAULT_GRID_PADDING,
    )
    .expect("build large renderer");
    large_renderer.resize(PixelSize { w, h });
    rebuild_text_frame(&mut large_renderer, &mut large, &device, &queue, "W");
    let (large_target, large_view) = render_target(&device, w, h);
    large_renderer.draw(&device, &queue, &large_view);
    let _ = read_rgba_pixels(&device, &queue, &large_target, w, h);

    // Bare redraw of the untouched first renderer.
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let (again_target, again_view) = render_target(&device, w, h);
    small_renderer.draw(&device, &queue, &again_view);
    let err = pollster::block_on(device.pop_error_scope());
    let again = read_rgba_pixels(&device, &queue, &again_target, w, h);

    assert!(err.is_none(), "wgpu validation error on redraw: {err:?}");
    assert_eq!(
        hash_pixels(&first),
        hash_pixels(&again),
        "a second font size must not disturb the first renderer's pixels — sharing one \
         atlas set between sizes overwrites its textures under coordinates it still holds"
    );
}

/// `rebind_glyph_atlases` must rebuild every pane bind group even when the
/// outgoing and incoming sets happen to share a texture generation — which
/// they normally do, because every set starts at zero. This is the precise
/// shape of the bug a generation-only staleness check would have.
#[test]
fn rebinding_to_a_fresh_atlas_set_rebuilds_pane_bind_groups() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available - skipping atlas rebind test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut pipeline_cache = PipelineCache::default();
    let pipelines = pipeline_cache.get(&device, format);
    let mut atlas_cache = GlyphAtlasCache::default();

    let mut small =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let small_atlases = atlas_cache.get(&device, &queue, format, &small);
    let mut renderer = Renderer::with_pipelines(
        &device,
        &queue,
        &pipelines,
        &small_atlases,
        &mut small,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 256, h: 128 });
    rebuild_text_frame(&mut renderer, &mut small, &device, &queue, "W");
    // Pane GPU state is created lazily at draw time, not at rebuild.
    let (_target, view) = render_target(&device, 256, 128);
    renderer.draw(&device, &queue, &view);
    let before = renderer.pane_bind_group_rebuild_counts();
    assert!(!before.is_empty(), "a pane must exist to rebind");

    let large =
        FontGrid::new(28.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let large_atlases = atlas_cache.get(&device, &queue, format, &large);
    assert_ne!(small_atlases.id(), large_atlases.id());

    renderer.rebind_glyph_atlases(&device, &large_atlases);
    let after = renderer.pane_bind_group_rebuild_counts();
    assert!(
        after.iter().zip(&before).all(|(now, was)| now > was),
        "every pane bind group must be rebuilt on a rebind: {before:?} -> {after:?}"
    );

    // Idempotent: callers may rebind every frame.
    renderer.rebind_glyph_atlases(&device, &large_atlases);
    assert_eq!(
        renderer.pane_bind_group_rebuild_counts(),
        after,
        "rebinding to the set already bound must not rebuild anything"
    );
}

/// Count pixels whose RGB differs from the top-left (background) pixel — a
/// cheap "did anything actually draw" oracle for a solid-background frame.
fn non_background_pixel_count(rgba: &[u8]) -> usize {
    let bg = &rgba[0..3];
    rgba.chunks_exact(4)
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
