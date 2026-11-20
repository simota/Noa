//! Headless timings for Cmd+hover link detection: what `noa-app`'s pointer
//! path runs on every `CursorMoved` while the link modifier is held
//! (`visible_logical_line`, then `url_at`, then `path_at` when no URL
//! matched). Each scenario hovers every cell of the viewport once per pass
//! and prints the per-call median/p99 plus an FNV-1a digest of every match,
//! so a before/after pair of binaries can be diffed for identical results as
//! well as compared for time.
//!
//!   cargo run --release -p noa-grid --example bench_hover -- [passes] [scenario]

use std::hint::black_box;
use std::time::Instant;

use noa_core::GridSize;
use noa_grid::Terminal;
use noa_vt::Stream;

const COLS: u16 = 200;
const ROWS: u16 = 50;

fn feed(bytes: &[u8]) -> Terminal {
    let mut terminal = Terminal::new(GridSize::new(COLS, ROWS));
    Stream::new().feed(bytes, &mut terminal);
    terminal
}

/// Compiler/log output: one path or URL every few words, short lines.
fn log_lines(lines: usize) -> Vec<u8> {
    let mut s = String::new();
    for i in 0..lines {
        s.push_str(&format!(
            "warning: unused variable `x{i}` --> crates/noa-grid/src/screen/mod{}.rs:{}:{} \
             see https://doc.rust-lang.org/rustc/lints/{i}.html for 'more' info\r\n",
            i % 7,
            i % 900 + 1,
            i % 80 + 1,
        ));
    }
    s.into_bytes()
}

/// Minified JSON with many links and no whitespace: one logical line wrapped
/// over far more rows than the hover window follows.
fn json_blob() -> Vec<u8> {
    let mut s = String::from("{");
    for i in 0..600 {
        s.push_str(&format!(
            r#""u{i}":"https://example.com/api/v1/items/{i}?page={i}&sort=asc","p{i}":"/usr/local/lib/{i}/x.rs","n{i}":"it's {i}","#
        ));
    }
    s.push_str("\"end\":0}\r\n");
    s.into_bytes()
}

/// Prose-ish text with apostrophes and quotes but no links.
fn prose(lines: usize) -> Vec<u8> {
    let mut s = String::new();
    for i in 0..lines {
        s.push_str(&format!(
            "It's {i}: the parser's \"state\" isn't what we'd expect; don't 'quote' me on it, \
             it's fine. Ain't nothing 'bout it that's wrong, y'all. ({i})\r\n"
        ));
    }
    s.into_bytes()
}

struct Scenario {
    name: &'static str,
    terminal: Terminal,
}

fn scenarios() -> Vec<Scenario> {
    let mut long = feed(&log_lines(100_000));
    long.primary.scroll_viewport_up(50_000);
    let mut json_sb = feed(&json_blob());
    json_sb.primary.scroll_viewport_up(200);
    vec![
        Scenario {
            name: "log_screen",
            terminal: feed(&log_lines(ROWS as usize)),
        },
        Scenario {
            name: "log_scrollback",
            terminal: long,
        },
        Scenario {
            name: "json_screen",
            terminal: feed(&json_blob()),
        },
        Scenario {
            name: "json_scrollback",
            terminal: json_sb,
        },
        Scenario {
            name: "prose_screen",
            terminal: feed(&prose(ROWS as usize)),
        },
        Scenario {
            name: "plain_wrapped",
            terminal: feed("x".repeat(COLS as usize * ROWS as usize * 3).as_bytes()),
        },
    ]
}

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *hash ^= u64::from(b);
        *hash = hash.wrapping_mul(0x100_0000_01b3);
    }
}

/// The pointer path's detection, minus the app's lock and existence probe.
fn hover(terminal: &Terminal, y: u16, x: u16, digest: &mut u64) {
    let Some(line) = terminal.active().visible_logical_line(y) else {
        return;
    };
    if let Some(url) = line.url_at(x) {
        fn_match(digest, &format!("u{y},{x}:{url:?}"));
        return;
    }
    if let Some(path) = line.path_at(x) {
        fn_match(digest, &format!("p{y},{x}:{path:?}"));
    }
}

fn fn_match(digest: &mut u64, s: &str) {
    fnv(digest, s.as_bytes());
}

fn main() {
    let mut args = std::env::args().skip(1);
    let passes: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(3);
    let filter = args.next();
    for scenario in scenarios() {
        if filter
            .as_deref()
            .is_some_and(|f| !scenario.name.contains(f))
        {
            continue;
        }
        let terminal = &scenario.terminal;
        let mut samples = Vec::new();
        let mut digest = 0xcbf2_9ce4_8422_2325u64;
        let start = Instant::now();
        for pass in 0..passes {
            for y in 0..ROWS {
                for x in 0..COLS {
                    let t = Instant::now();
                    let mut d = 0xcbf2_9ce4_8422_2325u64;
                    hover(terminal, y, x, &mut d);
                    samples.push(t.elapsed().as_nanos() as u64);
                    if pass == 0 {
                        fnv(&mut digest, &d.to_le_bytes());
                    }
                    black_box(d);
                }
            }
        }
        let total = start.elapsed();
        samples.sort_unstable();
        println!(
            "{:<16} calls={} total_ms={:.1} median_us={:.2} p99_us={:.2} max_us={:.2} digest={digest:016x}",
            scenario.name,
            samples.len(),
            total.as_secs_f64() * 1e3,
            samples[samples.len() / 2] as f64 / 1e3,
            samples[samples.len() * 99 / 100] as f64 / 1e3,
            *samples.last().unwrap() as f64 / 1e3,
        );
    }
}
