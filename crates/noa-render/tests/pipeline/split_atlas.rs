use super::shared::*;
use noa_core::{Color, DEFAULT_GRID_PADDING, PixelSize, Rgb};
use noa_font::FontGrid;
use noa_render::{DrawOp, PaneFrame, PaneId, PaneRect, Renderer, Theme, build_draw_plan};

#[test]
fn split_pipeline_syncs_same_frame_new_glyphs_for_two_panes() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping split atlas-ordering test");
        return;
    };
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 128, h: 32 });

    let left = snapshot_for_text("M");
    let right = snapshot_for_text("M");
    let layout = [
        (PaneId::new(1), PaneRect::new(0, 0, 64, 32)),
        (PaneId::new(2), PaneRect::new(65, 0, 63, 32)),
    ];
    let panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &left,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &right,
        },
    ];
    let initial_generation = font.mask_atlas_generation();

    renderer.rebuild_panes(&panes, &mut font, &Theme::new());
    if font.mask_atlas_generation() == initial_generation {
        eprintln!(
            "installed monospace font did not rasterize 'M' — skipping split atlas-ordering test"
        );
        return;
    }
    renderer.sync_atlas(&device, &queue, &mut font);

    assert_eq!(
        renderer.mask_atlas_seen_generation(),
        font.mask_atlas_generation()
    );

    let (_target, view) = render_target(&device, 128, 32);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw_panes(&device, &queue, &view, &layout, None, None, false);
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error during 2-pane same-frame atlas draw: {err:?}"
    );
}

#[test]
fn split_pipeline_draws_three_pane_plan_with_overlays_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping split draw-plan GPU test");
        return;
    };
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 160, h: 96 });

    let a = snapshot_for_text("A");
    let b = snapshot_for_text("B");
    let c = snapshot_for_text("C");
    let layout = [
        (PaneId::new(1), PaneRect::new(0, 0, 80, 96)),
        (PaneId::new(2), PaneRect::new(81, 0, 79, 47)),
        (PaneId::new(3), PaneRect::new(81, 48, 79, 48)),
    ];
    let panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &a,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &b,
        },
        PaneFrame {
            pane: layout[2].0,
            rect: layout[2].1,
            snapshot: &c,
        },
    ];
    let focused = layout[1].0;
    let plan = build_draw_plan(&layout, Some(focused), None, false);
    assert!(
        plan.iter()
            .any(|op| matches!(op, DrawOp::Dividers { rects } if !rects.is_empty())),
        "3-pane split plan should include same-pass divider geometry"
    );
    assert!(
        matches!(plan.last(), Some(DrawOp::FocusIndicator { pane, rects }) if *pane == focused && !rects.is_empty()),
        "focused split plan should include focus overlay geometry"
    );

    renderer.rebuild_panes(&panes, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);

    let (_target, view) = render_target(&device, 160, 96);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw_panes(&device, &queue, &view, &layout, Some(focused), None, false);
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error during 3-pane scissored draw with dividers: {err:?}"
    );
}

/// Regression: the z-band interleave binds the image pipeline inside a pane's
/// draw and only re-establishes the cell pipeline inside `draw_cell_range`.
/// When the final pane's trailing (decoration) cell range is empty — all-blank
/// cells emit no glyph/decoration instances — and it carries a z>=0 image, the
/// image pipeline is the last thing bound as the pane loop ends. The following
/// `Dividers` / `FocusIndicator` overlay draws must set the cell pipeline
/// themselves, or wgpu aborts inside the macOS delegate on a pipeline vs
/// bind-group mismatch. Focus the image pane so BOTH overlays draw after it.
#[test]
fn split_overlays_draw_after_final_pane_image_band_without_validation_error() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping overlay-after-image-band test");
        return;
    };
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 160, h: 96 });

    let a = snapshot_for_text("A");
    let b = snapshot_for_text("B");
    // Final pane: all-blank cells plus an opaque image at z=0 (above text), so
    // its trailing cell range is empty and the last bound pipeline before the
    // overlays is the image pipeline.
    let c = image_snapshot(4, 4, Color::Rgb(Rgb::new(0, 40, 0)), [0, 0, 255, 255], 0, 0);
    let layout = [
        (PaneId::new(1), PaneRect::new(0, 0, 80, 96)),
        (PaneId::new(2), PaneRect::new(81, 0, 79, 47)),
        (PaneId::new(3), PaneRect::new(81, 48, 79, 48)),
    ];
    let panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &a,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &b,
        },
        PaneFrame {
            pane: layout[2].0,
            rect: layout[2].1,
            snapshot: &c,
        },
    ];
    let focused = layout[2].0;
    let plan = build_draw_plan(&layout, Some(focused), None, false);
    assert!(
        plan.iter()
            .any(|op| matches!(op, DrawOp::Dividers { rects } if !rects.is_empty())),
        "3-pane split plan should include divider geometry"
    );
    assert!(
        matches!(plan.last(), Some(DrawOp::FocusIndicator { pane, rects }) if *pane == focused && !rects.is_empty()),
        "focusing the image pane should append its focus overlay after its image band"
    );

    renderer.rebuild_panes(&panes, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);

    let (_target, view) = render_target(&device, 160, 96);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.draw_panes(&device, &queue, &view, &layout, Some(focused), None, false);
    let err = pollster::block_on(device.pop_error_scope());

    assert!(
        err.is_none(),
        "wgpu validation error drawing split overlays after a final-pane image band: {err:?}"
    );
}

#[test]
fn split_pipeline_rebuilds_all_pane_bind_groups_after_atlas_reallocation() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping split atlas-reallocation test");
        return;
    };
    let mut font = FontGrid::new(220.0, noa_font::FontConfig::default())
        .expect("load a system monospace font");
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 512, h: 256 });

    let layout = [
        (PaneId::new(1), PaneRect::new(0, 0, 256, 256)),
        (PaneId::new(2), PaneRect::new(257, 0, 255, 256)),
    ];
    let first = snapshot_for_text("A");
    let second = snapshot_for_text("B");
    let initial_panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &first,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &second,
        },
    ];

    renderer.rebuild_panes(&initial_panes, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);
    let (_initial_target, initial_view) = render_target(&device, 512, 256);
    renderer.draw_panes(&device, &queue, &initial_view, &layout, None, None, false);

    let before_counts = renderer.pane_bind_group_rebuild_counts();
    assert_eq!(before_counts.len(), 2);
    let before_size = font.mask_atlas_size();

    let pressure = snapshot_for_text(&large_visible_glyph_string());
    let still_visible = snapshot_for_text("Z");
    let pressure_panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &pressure,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &still_visible,
        },
    ];
    renderer.rebuild_panes(&pressure_panes, &mut font, &Theme::new());
    if font.mask_atlas_size() == before_size {
        eprintln!(
            "large glyph pressure did not grow the atlas — skipping split atlas-reallocation test"
        );
        return;
    }

    let (_target, view) = render_target(&device, 512, 256);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.sync_atlas(&device, &queue, &mut font);
    let after_counts = renderer.pane_bind_group_rebuild_counts();
    renderer.draw_panes(&device, &queue, &view, &layout, None, None, false);
    let err = pollster::block_on(device.pop_error_scope());

    assert_eq!(after_counts.len(), before_counts.len());
    assert!(
        before_counts
            .iter()
            .zip(after_counts.iter())
            .all(|(before, after)| after > before),
        "atlas reallocation must rebuild every pane bind group: before={before_counts:?} after={after_counts:?}"
    );
    assert!(
        err.is_none(),
        "wgpu validation error after atlas reallocation draw: {err:?}"
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

#[test]
fn shared_font_atlas_syncs_to_multiple_renderers() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping multi-renderer atlas test");
        return;
    };
    let mut font =
        FontGrid::new(14.0, noa_font::FontConfig::default()).expect("load a system monospace font");
    let mut first = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build first renderer");
    let mut second = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build second renderer");

    let initial_generation = font.mask_atlas_generation();
    let glyph = font.get_or_raster('M');
    if glyph.atlas_size == [0, 0] || font.mask_atlas_generation() == initial_generation {
        eprintln!("installed monospace font did not rasterize 'M' — skipping atlas sync test");
        return;
    }
    let generation = font.mask_atlas_generation();

    first.sync_atlas(&device, &queue, &mut font);
    second.sync_atlas(&device, &queue, &mut font);

    assert_eq!(first.mask_atlas_seen_generation(), generation);
    assert_eq!(second.mask_atlas_seen_generation(), generation);
}

/// FM-09: force the color atlas alone to grow (many emoji glyphs) while
/// holding the mask atlas untouched, and assert every pane bind group is
/// still rebuilt. This is the case a "fixed the mask-atlas sync block, forgot
/// the color one" bug would slip through if `sync_atlas` duplicated its two
/// atlas blocks instead of sharing one code path.
#[test]
fn split_pipeline_rebuilds_bind_groups_after_color_only_atlas_reallocation() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping color-only atlas-reallocation test");
        return;
    };
    let mut font = FontGrid::new(200.0, noa_font::FontConfig::default())
        .expect("load a system monospace font");
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        &mut font,
        DEFAULT_GRID_PADDING,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 512, h: 256 });

    let layout = [
        (PaneId::new(1), PaneRect::new(0, 0, 256, 256)),
        (PaneId::new(2), PaneRect::new(257, 0, 255, 256)),
    ];
    let stable = snapshot_for_text("Z");
    let initial_panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &stable,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &stable,
        },
    ];

    renderer.rebuild_panes(&initial_panes, &mut font, &Theme::new());
    renderer.sync_atlas(&device, &queue, &mut font);
    let (_initial_target, initial_view) = render_target(&device, 512, 256);
    renderer.draw_panes(&device, &queue, &initial_view, &layout, None, None, false);

    let before_counts = renderer.pane_bind_group_rebuild_counts();
    assert_eq!(before_counts.len(), 2);
    let mask_size_before = font.mask_atlas_size();
    let mask_generation_before = font.mask_atlas_generation();
    let color_size_before = font.color_atlas_size();

    // Populate (and, hopefully, grow) the color atlas directly via
    // `get_or_raster` so we control exactly which characters are confirmed
    // color glyphs — this keeps the mask atlas provably untouched by this
    // pressure step, rather than hoping every candidate codepoint resolves
    // to a color face.
    let (emoji_text, color_atlas_grew) = build_color_atlas_pressure_string(&mut font);
    if !color_atlas_grew || emoji_text.is_empty() {
        eprintln!(
            "no color-capable emoji pressure grew the color atlas in this environment — \
             skipping color-only atlas-reallocation test"
        );
        return;
    }
    assert_eq!(
        font.mask_atlas_generation(),
        mask_generation_before,
        "building emoji-only pressure must not touch the mask atlas"
    );
    assert_eq!(
        font.mask_atlas_size(),
        mask_size_before,
        "building emoji-only pressure must not grow the mask atlas"
    );
    assert!(font.color_atlas_size() != color_size_before);

    let emoji_pressure = snapshot_for_text(&emoji_text);
    let pressure_panes = [
        PaneFrame {
            pane: layout[0].0,
            rect: layout[0].1,
            snapshot: &emoji_pressure,
        },
        PaneFrame {
            pane: layout[1].0,
            rect: layout[1].1,
            snapshot: &stable,
        },
    ];
    renderer.rebuild_panes(&pressure_panes, &mut font, &Theme::new());

    // Rendering the (already-rastered, cache-hit) pressure text must not
    // have touched the mask atlas either.
    assert_eq!(font.mask_atlas_generation(), mask_generation_before);
    assert_eq!(font.mask_atlas_size(), mask_size_before);

    let (_target, view) = render_target(&device, 512, 256);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    renderer.sync_atlas(&device, &queue, &mut font);
    let after_counts = renderer.pane_bind_group_rebuild_counts();
    renderer.draw_panes(&device, &queue, &view, &layout, None, None, false);
    let err = pollster::block_on(device.pop_error_scope());

    assert_eq!(after_counts.len(), before_counts.len());
    assert!(
        before_counts
            .iter()
            .zip(after_counts.iter())
            .all(|(before, after)| after > before),
        "color-only atlas reallocation must still rebuild every pane bind group (FM-09): \
         before={before_counts:?} after={after_counts:?}"
    );
    assert!(
        err.is_none(),
        "wgpu validation error after color-only atlas reallocation draw: {err:?}"
    );
}

/// Directly rasterize a range of emoji candidates, collecting only the ones
/// confirmed as color glyphs (`GlyphInfo.color`) into a pressure string.
/// Returns `(text, atlas_grew)`. Stops early once the color atlas has grown
/// and a reasonable number of glyphs were collected, to bound runtime.
fn build_color_atlas_pressure_string(font: &mut noa_font::FontGrid) -> (String, bool) {
    let before = font.color_atlas_size();
    let mut text = String::new();
    let mut grew = false;
    for ch in emoji_candidate_range() {
        let glyph = font.get_or_raster(ch);
        if glyph.color && glyph.atlas_size != [0, 0] {
            text.push(ch);
        }
        if font.color_atlas_size() != before {
            grew = true;
            if text.len() >= 64 {
                break;
            }
        }
    }
    (text, grew)
}

fn emoji_candidate_range() -> impl Iterator<Item = char> {
    ('\u{1F300}'..='\u{1F5FF}')
        .chain('\u{1F600}'..='\u{1F64F}')
        .chain('\u{1F680}'..='\u{1F6FF}')
        .chain('\u{1F900}'..='\u{1F9FF}')
}
