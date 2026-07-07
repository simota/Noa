use super::*;

#[test]
fn sync_atlas_uploads_rebuilt_font_grid_even_when_generation_restarts() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping rebuilt FontGrid atlas sync test");
        return;
    };
    let Some(mut first_font) = skip_font() else {
        return;
    };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut first_font,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    let first_identity = first_font.atlas_identity();
    assert_eq!(renderer.mask_atlas_seen_identity, first_identity);
    assert_eq!(renderer.color_atlas_seen_identity, first_identity);

    let mut rebuilt_font = match FontGrid::new(14.0, FontConfig::default()) {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return;
        }
    };
    assert_ne!(rebuilt_font.atlas_identity(), first_identity);
    assert_eq!(
        rebuilt_font.mask_atlas_generation(),
        renderer.mask_atlas_seen_generation(),
        "the regression requires a fresh FontGrid whose atlas generation restarts"
    );

    renderer.sync_atlas(&device, &queue, &mut rebuilt_font);

    assert_eq!(
        renderer.mask_atlas_seen_identity,
        rebuilt_font.atlas_identity(),
        "mask atlas sync must not skip a rebuilt FontGrid just because generation matches"
    );
    assert_eq!(
        renderer.color_atlas_seen_identity,
        rebuilt_font.atlas_identity(),
        "color atlas sync must not skip a rebuilt FontGrid just because generation matches"
    );
}

#[test]
fn atlas_eviction_epoch_forces_full_row_cache_rebuild() {
    // Regression: row-cache glyph instances store concrete atlas
    // coordinates. When FontGrid evicts a glyph slot, those coordinates
    // can later be reused by another glyph, so an otherwise-clean frame
    // must not reuse the old row instances.
    let mut font = match FontGrid::new_with_capped_atlas_for_tests(14.0, FontConfig::default(), 48)
    {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return;
        }
    };
    let theme = Theme::new();
    let mut cache = PaneRenderCache::empty();
    let snap = baseline_snapshot(['A', 'B', 'C']);
    let mut instances = Vec::new();

    let first = rebuild_pane_cached(&mut cache, &mut instances, &snap, &mut font, &theme, false);
    assert_eq!(
        first.rows_rebuilt, 3,
        "fresh pane cache should build every visible row"
    );
    instances.clear();

    let before_eviction = font.atlas_eviction_generation();
    for ch in ('!'..='~').chain('\u{3041}'..='\u{3096}') {
        font.get_or_raster(ch);
        if font.atlas_eviction_generation() > before_eviction {
            break;
        }
    }
    assert!(
        font.atlas_eviction_generation() > before_eviction,
        "capped atlas must evict after flooding distinct glyphs"
    );

    let second = rebuild_pane_cached(&mut cache, &mut instances, &snap, &mut font, &theme, false);
    assert!(
        second.rows_rebuilt >= 3,
        "atlas eviction must force a full row-cache rebuild even when row_dirty is false"
    );
}

#[test]
fn atlas_identity_change_forces_full_row_cache_rebuild() {
    // Regression: replacing FontGrid creates a fresh atlas whose eviction
    // generation restarts at the same value. Row-cache glyph instances still
    // contain coordinates from the old atlas identity, so clean rows must not
    // cache-hit after the replacement.
    let Some(mut font) = skip_font() else { return };
    let first_identity = font.atlas_identity();
    let first_generation = font.atlas_eviction_generation();
    let first_metrics = font.metrics();
    let theme = Theme::new();
    let mut cache = PaneRenderCache::empty();
    let snap = baseline_snapshot(['A', 'B', 'C']);
    let mut instances = Vec::new();

    let first = rebuild_pane_cached(&mut cache, &mut instances, &snap, &mut font, &theme, false);
    assert_eq!(
        first.rows_rebuilt, 3,
        "fresh pane cache should build every visible row"
    );
    instances.clear();

    let mut rebuilt_font = match FontGrid::new(14.0, FontConfig::default()) {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return;
        }
    };
    assert_ne!(
        rebuilt_font.atlas_identity(),
        first_identity,
        "the regression requires a fresh FontGrid identity"
    );
    assert_eq!(
        rebuilt_font.atlas_eviction_generation(),
        first_generation,
        "the regression requires matching eviction generations"
    );
    assert_eq!(
        rebuilt_font.metrics(),
        first_metrics,
        "the regression should isolate atlas identity from font metrics changes"
    );

    let second = rebuild_pane_cached(
        &mut cache,
        &mut instances,
        &snap,
        &mut rebuilt_font,
        &theme,
        false,
    );
    assert_eq!(
        second.rows_rebuilt, 3,
        "FontGrid identity changes must force a full row-cache rebuild even when row_dirty is false"
    );
}
