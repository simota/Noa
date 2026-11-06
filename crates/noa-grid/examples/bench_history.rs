//! Headless timings for work that scales with retained history rather than
//! with incoming bytes: scrolled-back frames, copy, search, reflow, plus the
//! two ingest shapes the bulk benches skip (image payloads and a text flood
//! with a live image placement).
//!
//! Every section prints an FNV-1a digest of its observable output, so a
//! before/after pair of binaries can be diffed for identical results as well
//! as compared for time.
//!
//!   cargo run --release -p noa-grid --example bench_history -- [lines] [cols] [rows] [section]
//!
//! `section` keeps only the sections whose name contains it (for profiling).

use std::hint::black_box;
use std::time::Instant;

use noa_core::GridSize;
use noa_grid::{Row, Terminal};
use noa_vt::Stream;

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *hash ^= u64::from(b);
        *hash = hash.wrapping_mul(0x100_0000_01b3);
    }
}

fn hash_rows(hash: &mut u64, rows: &[Row]) {
    for row in rows {
        for cell in &row.cells {
            fnv(hash, &(cell.ch as u32).to_le_bytes());
            fnv(hash, &cell.attrs.bits().to_le_bytes());
        }
    }
}

fn report(label: &str, mut samples_us: Vec<f64>, digest: u64) {
    samples_us.sort_by(f64::total_cmp);
    let at = |pct: usize| samples_us[(samples_us.len() - 1) * pct / 100];
    println!(
        "{label:<28} n={:<5} p50_us={:<12.1} p95_us={:<12.1} p99_us={:<12.1} digest={digest:016x}",
        samples_us.len(),
        at(50),
        at(95),
        at(99),
    );
}

const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

fn history_payload(lines: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(lines * 64);
    for i in 0..lines {
        if i % 16 == 0 {
            out.extend_from_slice(format!("\x1b[3{}m", i % 8).as_bytes());
        }
        out.extend_from_slice(format!("line {i:08} the quick brown fox jumps over\r\n").as_bytes());
        if i % 16 == 15 {
            out.extend_from_slice(b"\x1b[0m");
        }
    }
    out
}

fn filled(lines: usize, cols: u16, rows: u16) -> (Terminal, f64) {
    let payload = history_payload(lines);
    let mut term = Terminal::new(GridSize::new(cols, rows));
    let mut stream = Stream::new();
    let start = Instant::now();
    for chunk in payload.chunks(64 * 1024) {
        stream.feed(chunk, &mut term);
    }
    (term, start.elapsed().as_secs_f64() * 1e6)
}

fn bench_frames(term: &mut Terminal, label: &str, offset: usize, steps: usize) {
    let mut out = Vec::new();
    let mut samples = Vec::with_capacity(steps);
    let mut digest = FNV_BASIS;
    term.scroll_viewport_to_bottom();
    term.scroll_viewport_up(offset);
    for step in 0..steps {
        term.scroll_viewport_up(1);
        let start = Instant::now();
        term.active().visible_rows_into(&mut out);
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        if step % 64 == 0 {
            hash_rows(&mut digest, &out);
        }
    }
    term.scroll_viewport_to_bottom();
    report(label, samples, digest);
}

fn bench_copy(term: &mut Terminal) {
    let mut samples = Vec::new();
    let mut digest = FNV_BASIS;
    term.select_all();
    for _ in 0..9 {
        let start = Instant::now();
        let text = term.selected_text();
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        digest = FNV_BASIS;
        fnv(&mut digest, text.as_deref().unwrap_or("").as_bytes());
    }
    term.clear_selection();
    report("copy_select_all", samples, digest);
}

fn bench_search(term: &mut Terminal, query: &str, label: &str) {
    let mut samples = Vec::new();
    let mut digest = FNV_BASIS;
    let snapshot = term.active().search_snapshot();
    for _ in 0..5 {
        let start = Instant::now();
        let matches = snapshot.find_matches(query, || false).unwrap();
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        digest = FNV_BASIS;
        fnv(&mut digest, format!("{matches:?}").as_bytes());
    }
    report(label, samples, digest);
}

fn bench_reflow(lines: usize, cols: u16, rows: u16) {
    let mut samples = Vec::new();
    let mut digest = FNV_BASIS;
    let (mut term, _) = filled(lines, cols, rows);
    for narrow in [false, true, false, true, false, true] {
        let width = if narrow { cols / 2 } else { cols };
        let start = Instant::now();
        term.resize(GridSize::new(width, rows));
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        fnv(&mut digest, &term.scrollback_len().to_le_bytes());
        term.scroll_viewport_to_top();
        hash_rows(&mut digest, &term.active().visible_rows());
        term.scroll_viewport_to_bottom();
        hash_rows(&mut digest, &term.active().visible_rows());
    }
    report("reflow_half_width", samples, digest);
}

/// A chunked, direct-medium, base64 Kitty transmission: `a=t,f=32,s=,v=`.
fn kitty_payload(width: u32, height: u32) -> Vec<u8> {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let raw_len = (width * height * 4) as usize;
    let mut b64 = Vec::with_capacity(raw_len / 3 * 4 + 4);
    for i in 0..raw_len / 3 {
        let n = (i as u32).wrapping_mul(2_654_435_761) >> 8;
        for shift in [18, 12, 6, 0] {
            b64.push(B64[((n >> shift) & 63) as usize]);
        }
    }
    let mut out = Vec::with_capacity(b64.len() + b64.len() / 64);
    for (i, chunk) in b64.chunks(4096).enumerate() {
        let more = u8::from((i + 1) * 4096 < b64.len());
        if i == 0 {
            out.extend_from_slice(
                format!("\x1b_Ga=t,f=32,i=7,q=2,s={width},v={height},m={more};").as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

fn bench_kitty() {
    let payload = kitty_payload(1500, 1000);
    let mut samples = Vec::new();
    let mut digest = FNV_BASIS;
    for _ in 0..9 {
        let mut term = Terminal::new(GridSize::new(120, 40));
        let mut stream = Stream::new();
        let start = Instant::now();
        for chunk in payload.chunks(64 * 1024) {
            stream.feed(chunk, &mut term);
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        digest = FNV_BASIS;
        let image = term.kitty_images.get(7).expect("image stored");
        fnv(&mut digest, &image.rgba);
    }
    report("kitty_6MiB_rgba_transmit", samples, digest);
}

fn bench_flood_with_placement() {
    let flood = history_payload(400_000);
    let setup = b"\x1b_Ga=T,f=24,i=9,q=2,s=1,v=1;AAAA\x1b\\\r\n";
    for (label, prelude) in [
        ("flood_no_placement", &b""[..]),
        ("flood_with_placement", &setup[..]),
    ] {
        let mut samples = Vec::new();
        let mut digest = FNV_BASIS;
        for _ in 0..5 {
            let mut term = Terminal::new(GridSize::new(199, 60));
            let mut stream = Stream::new();
            stream.feed(prelude, &mut term);
            let start = Instant::now();
            for chunk in flood.chunks(64 * 1024) {
                stream.feed(chunk, &mut term);
            }
            samples.push(start.elapsed().as_secs_f64() * 1e6);
            digest = FNV_BASIS;
            hash_rows(&mut digest, &term.active().visible_rows());
            fnv(&mut digest, &term.scrollback_len().to_le_bytes());
        }
        report(label, samples, digest);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let lines: usize = args.next().map_or(300_000, |s| s.parse().unwrap());
    let cols: u16 = args.next().map_or(200, |s| s.parse().unwrap());
    let rows: u16 = args.next().map_or(60, |s| s.parse().unwrap());

    let (mut term, fill_us) = filled(lines, cols, rows);
    term.trim_memory();
    let retained = term.scrollback_len();
    println!("filled {lines} lines in {fill_us:.0} us, retained {retained} rows");

    let only = args.next().unwrap_or_default();
    let on = |name: &str| name.contains(&only);
    if on("frame") {
        bench_frames(&mut term, "frame_near_newest", 200, 2000);
        bench_frames(&mut term, "frame_mid_history", retained / 2, 2000);
    }
    if on("copy") {
        bench_copy(&mut term);
    }
    if on("search") {
        bench_search(&mut term, "fox", "search_sparse");
        bench_search(&mut term, "line 0000", "search_prefix");
        bench_search(&mut term, " ", "search_space");
    }
    if on("reflow") {
        bench_reflow(lines, cols, rows);
    }
    if on("kitty") {
        bench_kitty();
    }
    if on("flood") {
        bench_flood_with_placement();
    }
    black_box(term);
}
