//! End-to-end equivalence fuzz between the cached render path — snapshot
//! recycling (`FrameSnapshot::from_terminal_recycle`) + the per-row instance
//! cache + the scroll fast path, exactly as `noa-app`'s render loop drives
//! them — and a from-scratch all-dirty rebuild of the same terminal state.
//!
//! The caching layers are the only place a *persistent* visual divergence can
//! hide (a stale row keeps rendering until something re-dirties it), so any
//! pixel difference here is a real on-screen corruption bug. Runs a
//! deterministic pseudo-random stream of Claude-Code-flavored output (bold
//! CJK spans, SGR churn, CR rewrites, cursor addressing, scrolling) and
//! compares the two paths' rendered pixels after every frame.
//!
//! Skips gracefully when no GPU adapter is available (like `pipeline.rs`).

use noa_core::GridSize;
use noa_grid::Terminal;
use noa_render::{FrameSnapshot, FrameSnapshotRecycle, Renderer, Theme};
use noa_vt::Stream;

#[expect(
    dead_code,
    reason = "shared pipeline-test helpers; only device_queue is used here"
)]
#[path = "pipeline/shared.rs"]
mod shared;

const COLS: u16 = 40;
const ROWS: u16 = 8;

fn read_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let bytes_per_row = (w * 4).next_multiple_of(256);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (bytes_per_row * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = slice.get_mapped_range().to_vec();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let start = (y * bytes_per_row) as usize;
        out.extend_from_slice(&data[start..start + (w * 4) as usize]);
    }
    out
}

/// Build a full-copy snapshot without consuming dirty bits (ground truth).
fn fresh_snapshot(term: &Terminal) -> FrameSnapshot {
    let screen = term.active();
    let mut rows = Vec::new();
    for y in 0..screen.rows {
        let row = screen.visible_row(y).expect("row in range");
        rows.push(noa_grid::Row::from_cells(
            row.cells.clone(),
            row.wrapped,
            true,
        ));
    }
    let cols = screen.cols;
    let rows_n = screen.rows;
    let mut cursor = screen.cursor;
    if screen.viewport_offset() > 0 {
        cursor.visible = false;
    }
    FrameSnapshot {
        scroll_shift: 0,
        row_dirty: vec![true; rows.len()],
        rows,
        cursor,
        copy_cursor: None,
        colors: term.colors.clone(),
        selection: None,
        search: noa_grid::SearchState::default(),
        row_base: screen.visible_row_base(),
        abs_row_base: screen.rows_evicted() + screen.visible_row_base(),
        active_is_alt: term.active_is_alt,
        cols,
        rows_n,
        focused: true,
        cursor_blink_visible: true,
        hover_link: None,
        search_prompt: None,
        command_palette: None,
        confirm_dialog: None,
        preedit: None,
        image_placements: Vec::new(),
        images: Vec::new(),
    }
}

/// Deterministic xorshift so failures replay.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn cached_render_path_matches_full_rebuild() {
    let Some((device, queue)) = shared::device_queue() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    // Platform-default coding font: the equivalence property is
    // font-independent, and the default is the only family every machine has.
    let mut font = noa_font::FontGrid::new(16.0, noa_font::FontConfig::default()).expect("font");
    let metrics = font.metrics();
    let w = (metrics.cell_w as u32) * COLS as u32;
    let h = (metrics.cell_h as u32) * ROWS as u32;
    let format = wgpu::TextureFormat::Bgra8Unorm;
    let make_target = || {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    };
    let tex_cached = make_target();
    let tex_fresh = make_target();
    let view_cached = tex_cached.create_view(&wgpu::TextureViewDescriptor::default());
    let view_fresh = tex_fresh.create_view(&wgpu::TextureViewDescriptor::default());

    let theme = Theme::new();
    let mut cached_renderer = Renderer::new(
        &device,
        &queue,
        format,
        &mut font,
        noa_core::GridPadding::new(0.0, 0.0, 0.0, 0.0),
    )
    .expect("renderer");
    cached_renderer.resize(noa_core::PixelSize { w, h });
    let mut fresh_renderer = Renderer::new(
        &device,
        &queue,
        format,
        &mut font,
        noa_core::GridPadding::new(0.0, 0.0, 0.0, 0.0),
    )
    .expect("renderer");
    fresh_renderer.resize(noa_core::PixelSize { w, h });

    let mut term = Terminal::new(GridSize::new(COLS, ROWS));
    let mut stream = Stream::new();
    let mut recycle = FrameSnapshotRecycle::default();
    let mut rng = Rng(0x5eed_5eed_2026_0710);

    // Claude Code-flavored fragments: bold/italic CJK spans, SGR churn,
    // carriage-return rewrites, line feeds, cursor addressing, erases.
    let fragments: &[&str] = &[
        "\x1b[1mから制御可\x1b[0m",
        "\x1b[1m能にし、\x1b[0m",
        "plain text ",
        "\x1b[1;3mGhostty 1.3.0\x1b[0m の\r\n",
        "\x1b[38;5;117mosascript\x1b[39m",
        "\r\x1b[K\x1b[1mフルオブジェクトモデル\x1b[22mを狙う",
        "\r\n",
        "\x1b[2mdim 補足\x1b[0m",
        "\x1b[3;1H\x1b[1m上書きヘッダ\x1b[0m",
        "\x1b[999;1H", // jump to bottom row
        "改行\n",
        "\x1b[1mモ\x1b[0m\x1b[1mデ\x1b[0m\x1b[1mル\x1b[0m",
        "\x1b[4munder\x1b[24m",
        "\x1b[71;82R", // junk-ish CSI (ignored)
        "wide 混在 abc ゑ",
        "\x1b[A\x1b[K再描画した行\x1b[B\r",
    ];

    let mut mismatches = 0;
    for frame in 0..200 {
        // Feed 1-3 random fragments as one batch (one io-thread feed).
        for _ in 0..(1 + rng.pick(3)) {
            let frag = fragments[rng.pick(fragments.len())];
            stream.feed(frag.as_bytes(), &mut term);
        }

        // Cached path: exactly what noa-app's render loop does.
        let snapshot =
            FrameSnapshot::from_terminal_recycle(&mut term, std::mem::take(&mut recycle));
        cached_renderer.rebuild_cells(&snapshot, &mut font, &theme);
        cached_renderer.sync_atlas(&device, &queue, &mut font);
        cached_renderer.draw(&device, &queue, &view_cached);

        // Ground truth: full-copy all-dirty snapshot forces a full rebuild.
        let truth = fresh_snapshot(&term);
        fresh_renderer.rebuild_cells(&truth, &mut font, &theme);
        fresh_renderer.sync_atlas(&device, &queue, &mut font);
        fresh_renderer.draw(&device, &queue, &view_fresh);

        let a = read_pixels(&device, &queue, &tex_cached, w, h);
        let b = read_pixels(&device, &queue, &tex_fresh, w, h);
        if a != b {
            mismatches += 1;
            let diff = a
                .chunks(4)
                .zip(b.chunks(4))
                .enumerate()
                .filter(|(_, (x, y))| x != y)
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            let first = *diff.first().unwrap();
            let (px, py) = (first as u32 % w, first as u32 / w);
            eprintln!(
                "frame {frame}: {} differing pixels, first at ({px},{py}) cell ({},{})",
                diff.len(),
                px / metrics.cell_w as u32,
                py / metrics.cell_h as u32,
            );
            if mismatches > 5 {
                panic!("cached render path diverged from full rebuild");
            }
        }
        recycle = snapshot.into_recycle();
    }
    assert_eq!(
        mismatches, 0,
        "cached render path diverged from full rebuild"
    );
}
