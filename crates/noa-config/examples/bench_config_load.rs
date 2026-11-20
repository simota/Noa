//! Headless timings for startup config resolution: the full
//! `load_startup_config_from` path (read + include expansion + parse +
//! merge + finalize) on a synthetic config with one include, plus the
//! watcher's include-path discovery.
//!
//! Every section prints an FNV-1a digest of its observable output (the
//! resolved config's `Debug`, the diagnostics, the include list), so a
//! before/after pair of binaries can be diffed for identical results as
//! well as compared for time.
//!
//!   cargo run --release -p noa-config --example bench_config_load -- [iters] [keybinds]

use std::fmt::Write as _;
use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *hash ^= u64::from(b);
        *hash = hash.wrapping_mul(0x100_0000_01b3);
    }
}

fn report(label: &str, mut samples_us: Vec<f64>, digest: u64) {
    samples_us.sort_by(f64::total_cmp);
    let at = |pct: usize| samples_us[(samples_us.len() - 1) * pct / 100];
    println!(
        "{label:<24} n={:<6} p50_us={:<10.2} p95_us={:<10.2} p99_us={:<10.2} digest={digest:016x}",
        samples_us.len(),
        at(50),
        at(95),
        at(99),
    );
}

/// A config shaped like a well-used one: scalar keys of most value kinds,
/// comments and blank lines, a palette, font lists, `keybinds` keybind
/// lines, a couple of invalid values (diagnostics path), and an include.
fn main_config(keybinds: usize) -> String {
    let mut out = String::from(
        "# synthetic noa config\n\
         theme = light:Noa,dark:Noa\n\
         font-family = \"JetBrains Mono\"\n\
         font-family = Hiragino Sans\n\
         font-family-bold = \"JetBrains Mono\"\n\
         font-size = 14.5\n\
         font-feature = -calt\n\
         font-feature = +ss01\n\
         font-variation = wght=450\n\
         font-thicken = true\n\
         window-width = 120\n\
         window-height = 36\n\
         window-padding-x = 4,8\n\
         window-padding-y = 2\n\
         window-padding-balance = true\n\
         \n\
         background = #1e1e2e\n\
         foreground = cdd6f4\n\
         cursor-color = #f5e0dc\n\
         selection-background = #585b70\n\
         minimum-contrast = 1.1\n\
         cursor-style = bar\n\
         cursor-style-blink = false\n\
         background-opacity = 0.92\n\
         background-blur-radius = 20\n\
         scrollback-limit = 100000000\n\
         clipboard-read = ask\n\
         clipboard-write = allow\n\
         copy-on-select = clipboard\n\
         confirm-quit = false\n\
         macos-option-as-alt = left\n\
         macos-titlebar-style = tabs\n\
         quick-terminal-hotkey = ctrl+grave\n\
         quick-terminal-size = 40%\n\
         quick-terminal-position = top\n\
         sidebar-enabled = true\n\
         sidebar-width = 260\n\
         window-save-state = always\n\
         auto-approve = false\n\
         server-enable = false\n\
         not-a-real-key = 1\n\
         font-size = huge\n",
    );
    for i in 0..16 {
        let _ = writeln!(
            out,
            "palette = {i}=#{:02x}{:02x}{:02x}",
            i * 9,
            i * 7,
            i * 5
        );
    }
    let keys = ["a", "b", "c", "d", "t", "w", "n", "k", "left", "right"];
    for i in 0..keybinds {
        let key = keys[i % keys.len()];
        let _ = writeln!(out, "keybind = super+shift+{key}=new_split:right");
        if i.is_multiple_of(5) {
            out.push_str("# grouped keybinds\n\n");
        }
    }
    out.push_str("config-file = extra.conf\nconfig-file = ?missing-optional.conf\n");
    out
}

fn include_config() -> String {
    "# included\nfont-size = 15\ncursor-color = #ff00ff\nkeybind = ctrl+a=select_all\n\
     command-palette-entry = title:Reload,action:reload_config\n"
        .to_string()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let iters: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(5000);
    let keybinds: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(40);

    let dir = std::env::temp_dir().join("noa-bench-config");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let config = dir.join("config");
    let legacy = dir.join("config.toml");
    std::fs::write(&config, main_config(keybinds)).expect("write config");
    std::fs::write(dir.join("extra.conf"), include_config()).expect("write include");
    let config_noinc = dir.join("config-noinc");
    std::fs::write(&config_noinc, without_includes(&main_config(keybinds))).expect("write config");

    bench_parse(&config, &main_config(keybinds), iters);
    bench_load("load_startup_config", &config, &legacy, iters);
    bench_load("load_startup_config_noinc", &config_noinc, &legacy, iters);
    bench_include_paths(&config, iters);

    let _ = std::fs::remove_dir_all(&dir);
}

/// `source` with its `config-file` lines dropped: the common single-file
/// config.
fn without_includes(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.starts_with("config-file"))
        .flat_map(|line| [line, "\n"])
        .collect()
}

/// The in-memory parse alone: directive split, include expansion (the
/// includes still read from disk) and override building, no top-level read.
fn bench_parse(config: &Path, source: &str, iters: usize) {
    let without_includes = without_includes(source);
    for (label, source) in [
        ("parse_overrides", source),
        ("parse_overrides_noinc", &*without_includes),
    ] {
        let mut samples = Vec::with_capacity(iters);
        let mut digest = FNV_BASIS;
        for i in 0..iters {
            let start = Instant::now();
            let (overrides, diagnostics) =
                noa_config::parse_overrides(black_box(config), black_box(source));
            samples.push(start.elapsed().as_secs_f64() * 1e6);
            if i == 0 {
                fnv(&mut digest, format!("{overrides:?}").as_bytes());
                for diagnostic in &diagnostics {
                    fnv(&mut digest, diagnostic.message.as_bytes());
                }
            }
            black_box((overrides, diagnostics));
        }
        report(label, samples, digest);
    }
}

fn bench_load(label: &str, config: &Path, legacy: &Path, iters: usize) {
    let mut samples = Vec::with_capacity(iters);
    let mut digest = FNV_BASIS;
    for i in 0..iters {
        let start = Instant::now();
        let result = noa_config::load_startup_config_from(
            black_box(config),
            black_box(legacy),
            noa_config::ConfigOverrides::default(),
        );
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        if i == 0 {
            let (resolved, diagnostics) = result.expect("config resolves");
            fnv(&mut digest, format!("{resolved:?}").as_bytes());
            for diagnostic in &diagnostics {
                fnv(&mut digest, diagnostic.message.as_bytes());
            }
        } else {
            black_box(result.ok());
        }
    }
    report(label, samples, digest);
}

fn bench_include_paths(config: &Path, iters: usize) {
    let mut samples = Vec::with_capacity(iters);
    let mut digest = FNV_BASIS;
    for i in 0..iters {
        let start = Instant::now();
        let paths = noa_config::config_include_paths(black_box(config));
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        if i == 0 {
            for path in &paths {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                fnv(&mut digest, name.unwrap_or_default().as_bytes());
            }
        }
        black_box(paths);
    }
    report("config_include_paths", samples, digest);
}
