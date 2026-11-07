//! Ingest shapes the bulk-file benches skip: what TUIs and agent CLIs actually
//! emit (repaint frames, per-cell colors, hyperlinks, scroll regions, CJK with
//! styling, combining marks). Each section feeds a deterministic byte stream
//! into a fresh `Terminal` and prints an FNV-1a digest of the final screen, so
//! a before/after pair of binaries can be diffed for identical results as well
//! as compared for time.
//!
//!   cargo run --release -p noa-grid --example bench_vt_shapes -- [reps] [section]
//!
//! `section` keeps only the sections whose name contains it (for profiling).

use std::fmt::Write as _;
use std::time::Instant;

use noa_core::GridSize;
use noa_grid::Terminal;
use noa_vt::Stream;

const COLS: usize = 200;
const ROWS: usize = 60;
const TARGET_BYTES: usize = 32 << 20;

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

const WORDS: [&str; 8] = [
    "render",
    "cursor",
    "Terminal",
    "let",
    "scrollback",
    "0x1f4a9",
    "commit",
    "fn",
];
const CJK: [&str; 6] = ["端末", "描画", "高速化", "カーソル", "한국어", "測定"];

/// Full-screen repaint in a synchronized update: CUP per row, a truecolor SGR
/// per word, erase-to-EOL. The shape of Claude Code/ratatui/vim frames.
fn tui_repaint() -> Vec<u8> {
    let mut rng = Rng(0x1234_5678);
    let mut s = String::with_capacity(TARGET_BYTES + 65536);
    while s.len() < TARGET_BYTES {
        s.push_str("\x1b[?2026h\x1b[?25l");
        for row in 1..=ROWS {
            let _ = write!(s, "\x1b[{row};1H");
            for _ in 0..12 {
                let v = rng.next();
                let _ = write!(
                    s,
                    "\x1b[38;2;{};{};{}m{} ",
                    v & 0xff,
                    (v >> 8) & 0xff,
                    (v >> 16) & 0xff,
                    WORDS[(v >> 24) as usize % WORDS.len()]
                );
            }
            s.push_str("\x1b[0m\x1b[K");
        }
        s.push_str("\x1b[?25h\x1b[?2026l");
    }
    s.into_bytes()
}

/// A different 256-color SGR for every character (syntax-heavy cells, gradients).
fn sgr_per_cell() -> Vec<u8> {
    let mut rng = Rng(0x2345_6789);
    let mut s = String::with_capacity(TARGET_BYTES + 65536);
    while s.len() < TARGET_BYTES {
        for _ in 0..150 {
            let v = rng.next();
            let _ = write!(
                s,
                "\x1b[38;5;{}m{}",
                v & 0xff,
                (b'a' + (v >> 8) as u8 % 26) as char
            );
        }
        s.push_str("\x1b[0m\r\n");
    }
    s.into_bytes()
}

/// OSC 8 hyperlink spans plus shell-integration OSC 133/7/2 prompts.
fn osc_links() -> Vec<u8> {
    let mut rng = Rng(0x3456_789a);
    let mut s = String::with_capacity(TARGET_BYTES + 65536);
    let mut line = 0u32;
    while s.len() < TARGET_BYTES {
        let v = rng.next();
        for i in 0..4 {
            let _ = write!(
                s,
                "\x1b]8;;https://example.com/{}/{}\x1b\\link{i}\x1b]8;;\x1b\\ ",
                v % 64,
                i
            );
        }
        s.push_str("\r\n");
        line += 1;
        if line.is_multiple_of(32) {
            let _ = write!(
                s,
                "\x1b]133;D;0\x07\x1b]7;file://host/tmp/{}\x07\x1b]2;title {}\x07\x1b]133;A\x07$ \x1b]133;B\x07\x1b]133;C\x07\r\n",
                v % 16,
                v % 16
            );
        }
    }
    s.into_bytes()
}

/// tmux/vim-style scroll region churn: DECSTBM, RI/IL/DL, cursor moves.
fn scroll_region() -> Vec<u8> {
    let mut rng = Rng(0x4567_89ab);
    let mut s = String::with_capacity(TARGET_BYTES + 65536);
    while s.len() < TARGET_BYTES {
        let v = rng.next();
        let top = 2 + (v % 10) as usize;
        let bottom = ROWS - 2 - ((v >> 8) % 10) as usize;
        let _ = write!(s, "\x1b[{top};{bottom}r\x1b[{top};1H");
        for i in 0..20 {
            match (v >> (i % 16)) & 3 {
                0 => s.push_str("\x1b[2L"),
                1 => s.push_str("\x1b[2M"),
                2 => s.push_str("\x1bM"),
                _ => {
                    let _ = writeln!(s, "\x1b[{bottom};1H");
                }
            }
            let _ = write!(s, "{} {}", WORDS[i % WORDS.len()], v);
        }
        s.push_str("\x1b[r");
    }
    s.into_bytes()
}

/// Styled CJK runs in repaint frames (Japanese agent output).
fn cjk_tui() -> Vec<u8> {
    let mut rng = Rng(0x5678_9abc);
    let mut s = String::with_capacity(TARGET_BYTES + 65536);
    while s.len() < TARGET_BYTES {
        for row in 1..=ROWS {
            let _ = write!(s, "\x1b[{row};1H");
            for _ in 0..16 {
                let v = rng.next();
                let _ = write!(s, "\x1b[3{}m{}", v % 8, CJK[(v >> 8) as usize % CJK.len()]);
            }
            s.push_str("\x1b[0m\x1b[K");
        }
    }
    s.into_bytes()
}

/// Erase-heavy partial updates: ECH, EL, ED and DCH/ICH between short writes.
fn erase_edit() -> Vec<u8> {
    let mut rng = Rng(0x6789_abcd);
    let mut s = String::with_capacity(TARGET_BYTES + 65536);
    while s.len() < TARGET_BYTES {
        let v = rng.next();
        let row = 1 + v as usize % ROWS;
        let col = 1 + (v >> 8) as usize % COLS;
        let _ = write!(s, "\x1b[{row};{col}H");
        match (v >> 20) & 7 {
            0 => s.push_str("\x1b[K"),
            1 => s.push_str("\x1b[1K"),
            2 => s.push_str("\x1b[20X"),
            3 => s.push_str("\x1b[5P"),
            4 => s.push_str("\x1b[5@"),
            5 => s.push_str("\x1b[0J"),
            _ => {}
        }
        s.push_str(WORDS[(v >> 24) as usize % WORDS.len()]);
    }
    s.into_bytes()
}

/// Latin text with combining marks and emoji ZWJ clusters.
fn combining() -> Vec<u8> {
    let mut rng = Rng(0x789a_bcde);
    let mut s = String::with_capacity(TARGET_BYTES + 65536);
    while s.len() < TARGET_BYTES {
        for _ in 0..30 {
            let v = rng.next();
            match v % 4 {
                0 => s.push_str("e\u{301}a\u{308} "),
                1 => s.push_str("👩‍💻 "),
                2 => s.push_str("🇯🇵 "),
                _ => s.push_str(WORDS[(v >> 8) as usize % WORDS.len()]),
            }
        }
        s.push_str("\r\n");
    }
    s.into_bytes()
}

fn digest(term: &Terminal) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    };
    for row in &term.active().visible_rows() {
        for cell in &row.cells {
            mix(&(cell.ch as u32).to_le_bytes());
            mix(&cell.attrs.bits().to_le_bytes());
            mix(format!("{:?}{:?}", cell.fg, cell.bg).as_bytes());
        }
    }
    mix(&term.active().cursor.x.to_le_bytes());
    mix(&term.active().cursor.y.to_le_bytes());
    mix(term.title.as_bytes());
    hash
}

fn main() {
    let mut args = std::env::args().skip(1);
    let reps: usize = args.next().map_or(5, |s| s.parse().unwrap());
    let filter = args.next().unwrap_or_default();
    type Section = (&'static str, fn() -> Vec<u8>);
    let sections: [Section; 7] = [
        ("tui_repaint", tui_repaint),
        ("sgr_per_cell", sgr_per_cell),
        ("osc_links", osc_links),
        ("scroll_region", scroll_region),
        ("cjk_tui", cjk_tui),
        ("erase_edit", erase_edit),
        ("combining", combining),
    ];
    for (name, generate) in sections {
        if !name.contains(&filter) {
            continue;
        }
        let data = generate();
        let mib = data.len() as f64 / (1024.0 * 1024.0);
        let mut times = Vec::with_capacity(reps);
        let mut last = 0;
        for _ in 0..reps {
            let mut term = Terminal::new(GridSize::new(COLS as u16, ROWS as u16));
            let mut stream = Stream::new();
            let start = Instant::now();
            for chunk in data.chunks(64 * 1024) {
                stream.feed(chunk, &mut term);
                let _ = term.take_pending_writes();
            }
            times.push(start.elapsed().as_secs_f64());
            last = digest(&term);
        }
        times.sort_by(f64::total_cmp);
        print_section(name, mib, &times, last);
    }
    if "combining_read".contains(&filter) || filter.is_empty() {
        combining_read();
    }
}

fn print_section(name: &str, mib: f64, times: &[f64], last: u64) {
    println!(
        "{name:<14} {mib:>5.1} MiB  min={:>8.1}ms med={:>8.1}ms  {:>7.1} MiB/s digest={last:016x}",
        times[0] * 1e3,
        times[times.len() / 2] * 1e3,
        mib / times[times.len() / 2],
    );
}

/// Read side of combining-dense scrollback: materialize viewports through
/// history, the path that resolves each stored combining tail.
fn combining_read() {
    let data = combining();
    let mut term = Terminal::new(GridSize::new(COLS as u16, ROWS as u16));
    let mut stream = Stream::new();
    stream.feed(&data, &mut term);
    let mut out = Vec::new();
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut samples = Vec::new();
    term.scroll_viewport_to_bottom();
    for _ in 0..2000 {
        term.scroll_viewport_up(ROWS);
        let start = Instant::now();
        term.active().visible_rows_into(&mut out);
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        for row in &out {
            for cell in &row.cells {
                if cell.grapheme.is_some() {
                    hash = (hash ^ u64::from(cell.ch as u32)).wrapping_mul(0x100_0000_01b3);
                    for b in cell.combining().bytes() {
                        hash = (hash ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
                    }
                }
            }
        }
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "combining_read n={} p50_us={:.1} p95_us={:.1} p99_us={:.1} digest={hash:016x}",
        samples.len(),
        samples[samples.len() / 2],
        samples[samples.len() * 95 / 100],
        samples[samples.len() * 99 / 100],
    );
}
