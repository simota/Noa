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

fn large_visible_glyph_string() -> String {
    ('!'..='~')
        .chain('\u{00A1}'..='\u{017F}')
        .chain('\u{0370}'..='\u{03FF}')
        .chain('\u{0400}'..='\u{04FF}')
        .chain('\u{3041}'..='\u{3096}')
        .take(512)
        .collect()
}
