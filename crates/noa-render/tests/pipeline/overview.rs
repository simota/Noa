use super::shared::*;
use noa_core::{DEFAULT_GRID_PADDING, PixelSize};
use noa_font::FontGrid;
use noa_render::{OverviewThumbnailResources, Renderer};

#[test]
fn overview_blit_pipeline_draws_tile_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview blit GPU draw test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let scratch_size = PixelSize { w: 128, h: 64 };
    let tile_size = PixelSize { w: 64, h: 32 };
    let mut font =
        FontGrid::new(24.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
        .expect("build renderer");
    renderer.resize(scratch_size);
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "overview");

    let mut overview = OverviewThumbnailResources::for_renderer(
        &device,
        &queue,
        &renderer,
        scratch_size,
        tile_size,
        1,
        TEST_TITLE_BAR_H,
        TEST_CARD_COLOR,
    );
    assert_eq!(overview.format(), renderer.target_format());
    assert_eq!(overview.scratch_size(), scratch_size);
    assert_eq!(overview.tile_size(), tile_size);
    assert_eq!(overview.tile_count(), 1);

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, scratch_size, 0)
        .expect("blit existing renderer to overview tile");
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error during overview blit draw: {err:?}"
    );
}

#[test]
fn overview_blit_scratch_resizes_to_source_frame_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview blit scratch resize test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let initial_scratch_size = PixelSize { w: 64, h: 32 };
    let source_size = PixelSize { w: 160, h: 96 };
    let tile_size = PixelSize { w: 80, h: 50 };
    let mut font =
        FontGrid::new(24.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
        .expect("build renderer");
    renderer.resize(source_size);
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "source");

    let mut overview = OverviewThumbnailResources::for_renderer(
        &device,
        &queue,
        &renderer,
        initial_scratch_size,
        tile_size,
        1,
        TEST_TITLE_BAR_H,
        TEST_CARD_COLOR,
    );
    assert_eq!(overview.scratch_size(), initial_scratch_size);

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, source_size, 0)
        .expect("blit source-sized renderer to overview tile");
    let err = pollster::block_on(device.pop_error_scope());

    assert_eq!(overview.scratch_size(), source_size);
    assert!(
        err.is_none(),
        "wgpu validation error during overview scratch resize blit: {err:?}"
    );
}

#[test]
fn overview_blit_tile_pixel_hash_tracks_content_changes() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview blit pixel-hash test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let scratch_size = PixelSize { w: 160, h: 80 };
    let tile_size = PixelSize { w: 80, h: 40 };
    let mut font =
        FontGrid::new(28.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
        .expect("build renderer");
    renderer.resize(scratch_size);
    let mut overview = OverviewThumbnailResources::for_renderer(
        &device,
        &queue,
        &renderer,
        scratch_size,
        tile_size,
        1,
        TEST_TITLE_BAR_H,
        TEST_CARD_COLOR,
    );

    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "AAA");
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, scratch_size, 0)
        .expect("render first tile");
    let first = hash_pixels(&read_rgba_pixels(
        &device,
        &queue,
        overview.tile_texture_for_test(0).expect("tile exists"),
        tile_size.w,
        tile_size.h,
    ));

    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "AAA");
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, scratch_size, 0)
        .expect("render unchanged tile");
    let unchanged = hash_pixels(&read_rgba_pixels(
        &device,
        &queue,
        overview.tile_texture_for_test(0).expect("tile exists"),
        tile_size.w,
        tile_size.h,
    ));
    assert_eq!(
        first, unchanged,
        "unchanged tab content should produce the same overview tile hash"
    );

    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "WWW");
    overview
        .render_existing_renderer_to_tile(&device, &queue, &mut renderer, scratch_size, 0)
        .expect("render changed tile");
    let changed = hash_pixels(&read_rgba_pixels(
        &device,
        &queue,
        overview.tile_texture_for_test(0).expect("tile exists"),
        tile_size.w,
        tile_size.h,
    ));
    assert_ne!(
        unchanged, changed,
        "changed tab content should change the overview tile pixel hash"
    );
}

#[test]
fn overview_freshly_allocated_tiles_are_cleared_not_uninitialized() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview fresh-tile clear test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let scratch_size = PixelSize { w: 64, h: 32 };
    let tile_size = PixelSize { w: 64, h: 32 };
    let overview = OverviewThumbnailResources::new(
        &device,
        &queue,
        format,
        scratch_size,
        tile_size,
        2,
        TEST_TITLE_BAR_H,
        TEST_CARD_COLOR,
    );

    // Tile 0 is never rendered — it must still read back as a uniform card
    // fill rather than uninitialized GPU memory (the magenta-garbage bug).
    let fresh = read_rgba_pixels(
        &device,
        &queue,
        overview.tile_texture_for_test(0).expect("tile exists"),
        tile_size.w,
        tile_size.h,
    );
    let first = &fresh[..4];
    assert!(
        fresh.chunks_exact(4).all(|px| px == first),
        "freshly allocated overview tile must be a uniform clear, not uninitialized garbage"
    );

    // And that uniform fill must be the same card color the explicit
    // `clear_tile` placeholder path produces.
    overview.clear_tile(&device, &queue, 1);
    let cleared = read_rgba_pixels(
        &device,
        &queue,
        overview.tile_texture_for_test(1).expect("tile exists"),
        tile_size.w,
        tile_size.h,
    );
    assert_eq!(
        fresh, cleared,
        "freshly allocated tile must match the card-color clear"
    );
}

// Overview U1/Stage 2: `render_pane_into_tile_subrect` composites a pane's
// mirror into a *sub-rectangle* of the tab tile without clearing the rest, so
// several panes coexist in one tile. Proves (a) no wgpu validation error, and
// (b) the sub-rect target is real — swapping which content goes in the left vs
// right half changes the tile pixel hash, which could only happen if each
// composite landed in its own half rather than overwriting the whole tile.
#[test]
fn overview_pane_subrect_composition_places_panes_in_distinct_regions() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview sub-rect composition test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let scratch_size = PixelSize { w: 160, h: 80 };
    let tile_size = PixelSize { w: 120, h: 72 };
    let mut font =
        FontGrid::new(24.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
        .expect("build renderer");
    renderer.resize(scratch_size);
    let mut overview = OverviewThumbnailResources::for_renderer(
        &device,
        &queue,
        &renderer,
        scratch_size,
        tile_size,
        1,
        TEST_TITLE_BAR_H,
        TEST_CARD_COLOR,
    );

    // Two side-by-side pane cells inside the content region (below the title
    // band) — mirrors a horizontally-split tab laid out into the tile.
    let content_y = TEST_TITLE_BAR_H;
    let content_h = tile_size.h - TEST_TITLE_BAR_H;
    let half_w = tile_size.w / 2;
    let left = (0, content_y, half_w, content_h);
    let right = (half_w, content_y, tile_size.w - half_w, content_h);

    let hash_tile = |overview: &OverviewThumbnailResources| {
        hash_pixels(&read_rgba_pixels(
            &device,
            &queue,
            overview.tile_texture_for_test(0).expect("tile exists"),
            tile_size.w,
            tile_size.h,
        ))
    };

    device.push_error_scope(wgpu::ErrorFilter::Validation);

    let cleared = {
        overview.clear_tile(&device, &queue, 0);
        hash_tile(&overview)
    };

    // Arrangement 1: "AAA" on the left, "WWW" on the right.
    overview.clear_tile(&device, &queue, 0);
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "AAA");
    overview
        .render_pane_into_tile_subrect(&device, &queue, &mut renderer, scratch_size, 0, left)
        .expect("composite left pane");
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "WWW");
    overview
        .render_pane_into_tile_subrect(&device, &queue, &mut renderer, scratch_size, 0, right)
        .expect("composite right pane");
    let arrangement_1 = hash_tile(&overview);

    // Arrangement 2: swap the two — same content, opposite halves.
    overview.clear_tile(&device, &queue, 0);
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "WWW");
    overview
        .render_pane_into_tile_subrect(&device, &queue, &mut renderer, scratch_size, 0, left)
        .expect("composite left pane (swapped)");
    rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "AAA");
    overview
        .render_pane_into_tile_subrect(&device, &queue, &mut renderer, scratch_size, 0, right)
        .expect("composite right pane (swapped)");
    let arrangement_2 = hash_tile(&overview);

    let err = pollster::block_on(device.pop_error_scope());
    assert!(
        err.is_none(),
        "wgpu validation error during overview sub-rect composition: {err:?}"
    );
    assert_ne!(
        arrangement_1, cleared,
        "compositing panes into sub-rects must change the tile from its cleared state"
    );
    assert_ne!(
        arrangement_1, arrangement_2,
        "swapping which pane content lands in the left vs right sub-rect must change the tile — \
         proof the sub-rect target is honored, not a full-tile overwrite"
    );
}

#[test]
fn overview_blit_resources_drop_before_renderer_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overview blit teardown test");
        return;
    };
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let scratch_size = PixelSize { w: 96, h: 48 };
    let tile_size = PixelSize { w: 48, h: 24 };
    let mut font =
        FontGrid::new(18.0, noa_font::FontConfig::default()).expect("load a system monospace font");

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    {
        let mut renderer = Renderer::new(&device, &queue, format, &mut font, DEFAULT_GRID_PADDING)
            .expect("build renderer");
        renderer.resize(scratch_size);
        rebuild_text_frame(&mut renderer, &mut font, &device, &queue, "drop");

        {
            let mut overview = OverviewThumbnailResources::for_renderer(
                &device,
                &queue,
                &renderer,
                scratch_size,
                tile_size,
                1,
                TEST_TITLE_BAR_H,
                TEST_CARD_COLOR,
            );
            overview
                .render_existing_renderer_to_tile(&device, &queue, &mut renderer, scratch_size, 0)
                .expect("render before teardown");
        }
        drop(renderer);
    }
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll device after overview teardown");
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error during overview resources -> renderer teardown: {err:?}"
    );
}
