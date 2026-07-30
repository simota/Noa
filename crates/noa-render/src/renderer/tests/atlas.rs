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
    assert_eq!(renderer.mask_atlas_seen_identity(), first_identity);
    assert_eq!(renderer.color_atlas_seen_identity(), first_identity);

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
        renderer.mask_atlas_seen_identity(),
        rebuilt_font.atlas_identity(),
        "mask atlas sync must not skip a rebuilt FontGrid just because generation matches"
    );
    assert_eq!(
        renderer.color_atlas_seen_identity(),
        rebuilt_font.atlas_identity(),
        "color atlas sync must not skip a rebuilt FontGrid just because generation matches"
    );
}

/// `noa-app`'s font map keeps grids alive per pixel size, so the renderer can
/// see the atlas identity move **backwards**: a grid that was current, then
/// wasn't, then is again presents an identity numerically lower than the one
/// last seen. The sync guard compares identity by equality rather than
/// ordering, so this works; pin it, because an ordering comparison would
/// silently skip the re-upload.
///
/// Both grids here are the SAME pixel size on purpose. An atlas set belongs to
/// one size — feeding it another size's grid is the corruption
/// `SharedGlyphAtlases::sync`'s debug assertion now rejects, and the way a
/// window changes size is `rebind_glyph_atlases`, not a cross-size sync.
#[test]
fn sync_atlas_re_uploads_when_an_earlier_font_grid_comes_back() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping earlier-FontGrid atlas sync test");
        return;
    };
    let Some(mut first) = skip_font() else {
        return;
    };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut first,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    let first_identity = first.atlas_identity();

    // A second grid at the SAME pixel size becomes current.
    let mut second = match FontGrid::new(first.px_size(), FontConfig::default()) {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return;
        }
    };
    renderer.sync_atlas(&device, &queue, &mut second);
    assert_eq!(renderer.mask_atlas_seen_identity(), second.atlas_identity());

    // ...and now the earlier grid comes back, carrying a lower identity.
    assert!(
        first_identity < second.atlas_identity(),
        "identities are monotonic per construction, so the return really is backwards"
    );
    renderer.sync_atlas(&device, &queue, &mut first);

    assert_eq!(
        renderer.mask_atlas_seen_identity(),
        first_identity,
        "returning to an earlier FontGrid must re-upload its mask atlas"
    );
    assert_eq!(
        renderer.color_atlas_seen_identity(),
        first_identity,
        "returning to an earlier FontGrid must re-upload its color atlas"
    );
}

/// An atlas set belongs to exactly one pixel size. Syncing a grid of a
/// different size into it uploads one size's pixels under coordinates every
/// renderer bound to that set still holds — the corruption the per-size atlas
/// keying exists to prevent, reachable again the moment a caller passes the
/// wrong grid. It shipped once, in the overview thumbnail path, so the guard
/// is asserted here rather than left to review.
#[test]
#[should_panic(expected = "atlas set for")]
fn syncing_a_different_pixel_size_into_an_atlas_set_is_rejected() {
    let Some((device, queue)) = device_queue() else {
        // `should_panic` needs the panic, so make the skip path panic too.
        panic!("no wgpu adapter available — atlas set for skip");
    };
    let Some(mut small) = skip_font() else {
        panic!("no system monospace font — atlas set for skip");
    };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut small,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    let mut large =
        FontGrid::new(small.px_size() * 2.0, FontConfig::default()).expect("build a second size");
    renderer.sync_atlas(&device, &queue, &mut large);
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
