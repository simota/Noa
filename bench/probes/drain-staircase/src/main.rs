//! Drain-staircase profiler (wish #4 / PTY drain throughput).
//!
//! Measures the *producer-side PTY write-completion wall-clock* — the same
//! quantity `tbench` reports — and decomposes it against two other lenses so
//! the rate-limiting stage can be read off directly.
//!
//! Staircase stages:
//!   S0  no reader on the master        (reference: producer stalls at buf cap)
//!   S1  read + discard                 (raw drain ceiling)
//!   S2  read + noa-vt parse, no-op Handler          (+ parser)
//!   S3  read + parse + real Terminal apply, scrollback OFF   (+ state mutate)
//!   S4  S3 with scrollback ON (default 10MB)                 (+ scrollback pack)
//!
//! Three lenses:
//!   proc   pure in-memory processing (no pty at all): the *ceiling* of the
//!          processing layer for S2/S3/S4. If real-drain ≈ proc, drain is
//!          processing-bound; if proc ≫ real-drain, it is pipeline-bound.
//!   real   noa_pty::Pty spawns a `cat`-style producer; Noa's real reader
//!          thread drains the master and a consumer thread parses+applies
//!          under an Arc<Mutex<Terminal>> exactly like io_thread.rs. The
//!          authoritative production drain (reproduces tbench). `--contend`
//!          adds a ~120Hz FrameSnapshot-style lock contender to S4.
//!   naive  one-thread blocking read+discard on an in-process openpty — the
//!          floor a naive serial reader hits (illustrates why Noa splits the
//!          read onto its own O_NONBLOCK-coalescing reader thread).
//!
//! Uses only the public APIs of noa-vt / noa-grid / noa-pty.

use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use noa_core::GridSize;
use noa_grid::Terminal;
use noa_vt::{
    Charset, CharsetSlot, CursorStyle, DaKind, DsrKind, EraseDisplay, EraseLine, Handler, SgrAttr,
    Stream,
};
use parking_lot::Mutex;

const CHUNK: usize = 64 * 1024;

// ── workloads ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Workload {
    Plain,
    Ansi,
    Scroll,
}

impl Workload {
    fn name(self) -> &'static str {
        match self {
            Workload::Plain => "plain",
            Workload::Ansi => "ansi",
            Workload::Scroll => "scroll",
        }
    }
    fn all() -> [Workload; 3] {
        [Workload::Plain, Workload::Ansi, Workload::Scroll]
    }
}

/// A reusable ~4 MiB template. The producer writes it repeatedly (last write
/// truncated) to reach the requested total, so content is deterministic and
/// identical across every stage/lens.
fn template(w: Workload) -> Vec<u8> {
    const TARGET: usize = 4 * 1024 * 1024;
    let mut out = Vec::with_capacity(TARGET + 4096);
    match w {
        Workload::Plain => {
            let lines: [&str; 4] = [
                "The quick brown fox jumps over the lazy dog while 0123456789 counts along here.",
                "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor xx.",
                "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 plain ascii row #.",
                "noa faithful ghostty clone drain staircase printable throughput sample line here.",
            ];
            let mut i = 0usize;
            while out.len() < TARGET {
                out.extend_from_slice(lines[i % lines.len()].as_bytes());
                out.push(b'\n');
                i += 1;
            }
        }
        Workload::Ansi => {
            let palette: [&str; 6] = [
                "\x1b[38;5;196;48;5;17;1m",
                "\x1b[38;5;46;48;5;52;3m",
                "\x1b[38;5;51;48;5;22;4m",
                "\x1b[38;5;226;48;5;17;7m",
                "\x1b[38;5;201;48;5;52;1m",
                "\x1b[38;5;129;48;5;22;4m",
            ];
            let payload = "colored SGR churn 0123456789 ABCDEFGHIJ ~!@#$%^&*() the quick brown fox";
            let mut i = 0usize;
            while out.len() < TARGET {
                out.extend_from_slice(palette[i % palette.len()].as_bytes());
                out.extend_from_slice(payload.as_bytes());
                out.extend_from_slice(b"\x1b[0m\n");
                i += 1;
            }
        }
        Workload::Scroll => {
            let payload = "scroll row ###\n";
            let mut i = 0usize;
            while out.len() < TARGET {
                if i.is_multiple_of(512) {
                    out.extend_from_slice(b"\x1b[3;22r");
                }
                out.extend_from_slice(payload.as_bytes());
                if i % 512 == 511 {
                    out.extend_from_slice(b"\x1b[r");
                }
                i += 1;
            }
        }
    }
    // DS_CRLF=1: model a cooked tty (ONLCR) the way tbench's `cat > /dev/tty`
    // actually reaches the terminal — every LF arrives as CR LF, so each line
    // restarts at column 0 instead of drifting/wrapping under raw LF.
    if std::env::var_os("DS_CRLF").is_some() {
        let mut crlf = Vec::with_capacity(out.len() + out.len() / 16);
        for &b in &out {
            if b == b'\n' {
                crlf.push(b'\r');
            }
            crlf.push(b);
        }
        return crlf;
    }
    out
}

// ── no-op Handler (S2: parser-only) ────────────────────────────────────

struct NoOp;

#[rustfmt::skip]
impl Handler for NoOp {
    fn print(&mut self, _c: char) {}
    fn print_str(&mut self, _s: &str) {}
    fn execute_c0(&mut self, _b: u8) {}
    fn cursor_up(&mut self, _n: u16) {}
    fn cursor_down(&mut self, _n: u16) {}
    fn cursor_forward(&mut self, _n: u16) {}
    fn cursor_backward(&mut self, _n: u16) {}
    fn cursor_position(&mut self, _r: u16, _c: u16) {}
    fn cursor_col_abs(&mut self, _c: u16) {}
    fn cursor_row_abs(&mut self, _r: u16) {}
    fn erase_display(&mut self, _m: EraseDisplay) {}
    fn erase_line(&mut self, _m: EraseLine) {}
    fn set_attributes(&mut self, _a: &[SgrAttr]) {}
    fn set_mode(&mut self, _v: u16, _ansi: bool, _on: bool) {}
    fn carriage_return(&mut self) {}
    fn linefeed(&mut self) {}
    fn tab(&mut self, _n: u16) {}
    fn reverse_index(&mut self) {}
    fn save_cursor(&mut self) {}
    fn restore_cursor(&mut self) {}
    fn full_reset(&mut self) {}
    fn device_attributes(&mut self, _k: DaKind) {}
    fn device_status_report(&mut self, _k: DsrKind) {}
    fn designate_charset(&mut self, _s: CharsetSlot, _c: Charset) {}
    fn set_cursor_style(&mut self, _s: CursorStyle) {}
}

// ── stage ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    S1Discard,
    S2Parse,
    S3ApplyNoSb,
    S4ApplySb,
}

fn stage_name(s: Stage) -> &'static str {
    match s {
        Stage::S1Discard => "S1 read+discard",
        Stage::S2Parse => "S2 parse(noop)",
        Stage::S3ApplyNoSb => "S3 apply sb-off",
        Stage::S4ApplySb => "S4 apply sb-on",
    }
}

fn make_terminal(stage: Stage, grid: GridSize) -> Option<Terminal> {
    match stage {
        Stage::S3ApplyNoSb => {
            let mut t = Terminal::new(grid);
            t.set_scrollback_limit_bytes(0);
            Some(t)
        }
        Stage::S4ApplySb => {
            let mut t = Terminal::new(grid);
            // Experiment knob: override the scrollback retention limit to
            // isolate eviction/limit-coupled costs (default 10MiB stays).
            if let Some(mb) = std::env::var("DS_SB_LIMIT_MB")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
            {
                t.set_scrollback_limit_bytes(mb * 1024 * 1024);
            }
            Some(t)
        }
        _ => None,
    }
}

// ── libc pty helpers ───────────────────────────────────────────────────

struct Pair {
    master: RawFd,
    slave: RawFd,
}

/// openpty with a raw termios (OPOST/ONLCR off) so bytes written to the slave
/// reach the master unchanged — no `\n`→`\r\n` inflation to skew byte counts.
fn open_raw_pty(cols: u16, rows: u16) -> Pair {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    unsafe { libc::cfmakeraw(&mut term) };
    let mut win = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            &mut term as *mut libc::termios,
            &mut win as *mut libc::winsize,
        )
    };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
    Pair { master, slave }
}

fn set_nonblocking(fd: RawFd, on: bool) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        let flags = if on {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        libc::fcntl(fd, libc::F_SETFL, flags);
    }
}

fn write_all(fd: RawFd, buf: &[u8]) {
    let mut off = 0;
    while off < buf.len() {
        let n = unsafe {
            libc::write(
                fd,
                buf[off..].as_ptr() as *const libc::c_void,
                buf.len() - off,
            )
        };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            panic!("write failed: {e}");
        }
        off += n as usize;
    }
}

fn close_fd(fd: RawFd) {
    unsafe {
        libc::close(fd);
    }
}

// ── S0: buffer-capacity reference ──────────────────────────────────────

/// No reader on the master: set the slave non-blocking, write 1 KiB at a time
/// until EAGAIN, and report the bytes the producer could push before stalling
/// (the kernel pty buffer). Proves drain is entirely reader-gated.
fn run_s0_buffer_probe(grid: GridSize) -> usize {
    let pair = open_raw_pty(grid.cols, grid.rows);
    set_nonblocking(pair.slave, true);
    let block = vec![b'x'; 1024];
    let mut written = 0usize;
    loop {
        let n = unsafe {
            libc::write(
                pair.slave,
                block.as_ptr() as *const libc::c_void,
                block.len(),
            )
        };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            match e.kind() {
                std::io::ErrorKind::WouldBlock => break,
                std::io::ErrorKind::Interrupted => continue,
                _ => panic!("s0 write failed: {e}"),
            }
        }
        if n == 0 {
            break;
        }
        written += n as usize;
        if written > 16 * 1024 * 1024 {
            break;
        }
    }
    close_fd(pair.master);
    close_fd(pair.slave);
    written
}

// ── naive one-thread blocking read+discard (floor) ─────────────────────

/// One thread writes `total` bytes to the slave (blocking) while THIS thread
/// does blocking 64 KiB reads on the master and discards. Read and process
/// (here: nothing) share a thread, and a blocking read returns whatever the
/// tiny pty buffer holds — so producer↔consumer ping-pong dominates. Returns
/// producer write-completion wall-clock.
fn run_naive_discard(tmpl: &[u8], total: usize, grid: GridSize) -> Duration {
    let pair = open_raw_pty(grid.cols, grid.rows);
    let (master, slave) = (pair.master, pair.slave);
    let tmpl_owned = tmpl.to_vec();
    let producer = std::thread::spawn(move || {
        let start = Instant::now();
        let mut written = 0usize;
        while written < total {
            let end = (written + tmpl_owned.len()).min(total);
            write_all(slave, &tmpl_owned[..end - written]);
            written = end;
        }
        start.elapsed()
    });
    let mut buf = vec![0u8; CHUNK];
    let mut read_total = 0usize;
    while read_total < total {
        let n = unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            panic!("read failed: {e}");
        }
        if n == 0 {
            break;
        }
        read_total += n as usize;
    }
    let elapsed = producer.join().unwrap();
    close_fd(master);
    close_fd(slave);
    elapsed
}

// ── proc: pure in-memory processing ceiling ────────────────────────────

/// Feed `total` bytes of `tmpl` (repeated, in 64 KiB slices to match chunk
/// granularity) through one long-lived Stream + one Terminal, timed, with no
/// pty/IO at all. The ceiling of the processing layer for this stage/workload.
fn run_proc(stage: Stage, tmpl: &[u8], total: usize, grid: GridSize) -> Duration {
    let mut stream = Stream::new();
    let mut noop = NoOp;
    let mut terminal = make_terminal(stage, grid);
    let start = Instant::now();
    let mut written = 0usize;
    while written < total {
        let end = (written + tmpl.len()).min(total);
        let slice = &tmpl[..end - written];
        for c in slice.chunks(CHUNK) {
            match stage {
                Stage::S1Discard => {}
                Stage::S2Parse => stream.feed(c, &mut noop),
                Stage::S3ApplyNoSb | Stage::S4ApplySb => stream.feed(c, terminal.as_mut().unwrap()),
            }
        }
        written = end;
    }
    let _ = std::hint::black_box(&terminal);
    start.elapsed()
}

/// Profiling driver: feed one workload's template through one long-lived
/// Stream + Terminal in a tight loop for `secs` seconds, so an external
/// sampler (`sample <pid>` / xctrace Time Profiler) can attribute apply-stage
/// CPU by function. Single owner thread (scrollback pack workers run on their
/// own threads, so `sample` separates owner vs worker time). No IO, no timing
/// overhead in the loop. Harness-only; touches no crate code.
fn run_proc_profile(stage: Stage, w: Workload, secs: u64, grid: GridSize) {
    let tmpl = template(w);
    let mut stream = Stream::new();
    let mut terminal = make_terminal(stage, grid).expect("profile stage needs a terminal");
    eprintln!(
        "proc-profile: {} {} {}x{} for {}s — attach `sample {} {} -f out.txt` now",
        stage_name(stage),
        w.name(),
        grid.cols,
        grid.rows,
        secs,
        std::process::id(),
        secs
    );
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut fed: u64 = 0;
    while Instant::now() < deadline {
        // Feed the whole template ~a few times between clock reads so the
        // Instant::now() check is a negligible share of the loop.
        for _ in 0..8 {
            for c in tmpl.chunks(CHUNK) {
                stream.feed(c, &mut terminal);
            }
            fed += tmpl.len() as u64;
        }
    }
    let _ = std::hint::black_box(&terminal);
    eprintln!(
        "proc-profile done: fed {} MiB ({:.0} MB/s avg)",
        fed / (1024 * 1024),
        fed as f64 / (1024.0 * 1024.0) / secs as f64
    );
}

// ── real: production noa_pty pipeline ──────────────────────────────────

/// Spawn a `cat`-style producer through `noa_pty::Pty` (its real reader thread
/// drains the master) and drain `event_rx` on this thread, parsing+applying
/// each chunk under an Arc<Mutex<Terminal>> with a fair unlock, exactly like
/// io_thread.rs's `feed_chunk_fair`. Returns first-data → EOF wall-clock.
/// `contend` optionally runs a ~120Hz FrameSnapshot-style lock contender.
fn run_real(stage: Stage, tmpfile: &str, total: usize, grid: GridSize, contend: bool) -> Duration {
    use noa_pty::{Pty, PtyConfig, PtyEvent};

    let cfg = PtyConfig {
        size: grid,
        shell: None,
        command: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("stty raw -opost 2>/dev/null; exec cat {tmpfile}"),
        ]),
        cwd: None,
        term: "xterm-256color".to_string(),
        login: false,
        shell_integration: false,
    };
    let pty = Pty::spawn(cfg).expect("spawn pty");

    let mut stream = Stream::new();
    let mut noop = NoOp;
    let terminal = make_terminal(stage, grid).map(|t| Arc::new(Mutex::new(t)));

    let stop = Arc::new(AtomicBool::new(false));
    let contender = if contend {
        terminal.as_ref().map(|term| {
            let term = Arc::clone(term);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    {
                        let t = term.lock();
                        let s = t.active();
                        let n = s.rows as u64 * s.cols as u64;
                        let mut acc = 0u64;
                        for i in 0..n {
                            acc = acc.wrapping_add(i);
                        }
                        std::hint::black_box(acc);
                    }
                    std::thread::sleep(Duration::from_micros(8333));
                }
            })
        })
    } else {
        None
    };

    let mut first_data: Option<Instant> = None;
    let mut seen = 0usize;
    let timeout = Duration::from_secs(60);
    loop {
        match pty.event_rx().recv_timeout(timeout) {
            Ok(PtyEvent::Data(chunk)) => {
                if first_data.is_none() {
                    first_data = Some(Instant::now());
                }
                seen += chunk.len();
                match stage {
                    Stage::S1Discard => {}
                    Stage::S2Parse => stream.feed(chunk.as_ref(), &mut noop),
                    Stage::S3ApplyNoSb | Stage::S4ApplySb => {
                        let mut t = terminal.as_ref().unwrap().lock();
                        stream.feed(chunk.as_ref(), &mut *t);
                        parking_lot::MutexGuard::unlock_fair(t);
                    }
                }
            }
            Ok(PtyEvent::Exit(_)) | Ok(PtyEvent::Error(_)) => break,
            Err(_) => break,
        }
    }
    let elapsed = first_data.map(|t| t.elapsed()).unwrap_or(timeout);
    stop.store(true, Ordering::Relaxed);
    if let Some(h) = contender {
        let _ = h.join();
    }
    if seen < total * 9 / 10 {
        eprintln!(
            "  [warn] real {} drained {} / {} bytes",
            stage_name(stage),
            seen,
            total
        );
    }
    elapsed
}

// ── tbench-faithful: real fixture, real ONLCR, one warm session, N runs ──

/// Reproduces tbench's own measurement architecture as closely as this
/// harness can from the consumer side: ONE `noa_pty::Pty` session (one
/// cooked, default-termios pty — no `stty raw -opost`, so the kernel's real
/// ONLCR expands `\n`→`\r\n` exactly as it does for a real Noa window) whose
/// child shell sequentially `cat`s the *actual* tbench fixture file
/// `warmup + measured` times, each iteration followed by a BEL (`\a`)
/// sentinel the consumer uses to mark run boundaries (BEL is a no-op
/// `execute_c0` for `Terminal` — it cannot appear in the plain-text fixture,
/// so there is no risk of a false boundary). One long-lived `Terminal`
/// (scrollback ON, the real default) absorbs all runs in sequence — unlike
/// `measure()`'s per-rep fresh `Pty`+`Terminal`, this models the same warm,
/// cumulative-state session tbench's own `--runs 20` exercises against a
/// single live window. Returns the `measured` per-run wall-clock durations
/// (warmup runs dropped), in run order.
fn run_tbench_faithful(
    fixture_path: &str,
    grid: GridSize,
    warmup: usize,
    measured: usize,
    raw_tty: bool,
    stage: Stage,
) -> Vec<Duration> {
    use noa_pty::{Pty, PtyConfig, PtyEvent};

    let total_runs = warmup + measured;
    let stty_prefix = if raw_tty {
        "stty raw -opost 2>/dev/null; "
    } else {
        ""
    };
    let cfg = PtyConfig {
        size: grid,
        shell: None,
        command: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!(
                "{stty_prefix}i=0; while [ $i -lt {total_runs} ]; do cat '{fixture_path}'; printf '\\a'; i=$((i+1)); done"
            ),
        ]),
        cwd: None,
        term: "xterm-256color".to_string(),
        login: false,
        shell_integration: false,
    };
    let pty = Pty::spawn(cfg).expect("spawn pty");

    let mut stream = Stream::new();
    let mut noop = NoOp;
    let terminal = make_terminal(stage, grid).map(|t| Arc::new(Mutex::new(t)));

    // boundaries[0] = first byte of run 1 arrives; boundaries[k] = the k-th
    // BEL sentinel arrives (end of run k). Duration of run k = boundaries[k]
    // - boundaries[k-1].
    let mut boundaries: Vec<Instant> = Vec::with_capacity(total_runs + 1);
    let timeout = Duration::from_secs(120);
    loop {
        match pty.event_rx().recv_timeout(timeout) {
            Ok(PtyEvent::Data(chunk)) => {
                let bytes = chunk.as_ref();
                if boundaries.is_empty() {
                    boundaries.push(Instant::now());
                }
                match stage {
                    Stage::S1Discard => {}
                    Stage::S2Parse => stream.feed(bytes, &mut noop),
                    Stage::S3ApplyNoSb | Stage::S4ApplySb => {
                        let mut t = terminal.as_ref().unwrap().lock();
                        stream.feed(bytes, &mut *t);
                        parking_lot::MutexGuard::unlock_fair(t);
                    }
                }
                let bel_count = bytes.iter().filter(|&&b| b == 0x07).count();
                if bel_count > 0 {
                    let now = Instant::now();
                    for _ in 0..bel_count {
                        boundaries.push(now);
                    }
                }
                if boundaries.len() > total_runs {
                    break;
                }
            }
            Ok(PtyEvent::Exit(_)) | Ok(PtyEvent::Error(_)) => break,
            Err(_) => break,
        }
    }
    let _ = std::hint::black_box(&terminal);
    let mut durs = Vec::new();
    for i in 1..boundaries.len() {
        durs.push(boundaries[i].duration_since(boundaries[i - 1]));
    }
    if durs.len() > warmup {
        durs.split_off(warmup)
    } else {
        eprintln!(
            "  [warn] tbench-faithful: only {} of {} runs completed",
            durs.len(),
            total_runs
        );
        durs
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

/// `malloc_zone_pressure_relief(all, everything)` — return already-free
/// heap pages to the OS without touching live state (models the app's
/// mid-burst relief timer).
fn pressure_relief() {
    #[cfg(target_os = "macos")]
    // SAFETY: documented-safe with NULL zone (all zones) and 0 goal.
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

/// Resident-set size of this process in bytes (mach task_info).
fn current_rss() -> usize {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps rss");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<usize>()
        .unwrap_or(0)
        * 1024
}

/// Memory-retention suite (bonus-cycle oracle): ONE warm Terminal + ONE
/// cooked pty session absorbs `suites` repetitions of the full fixture
/// rotation (each fixture `runs` times, BEL-delimited), with the app's
/// idle `trim_memory` modeled between fixtures — printing process RSS at
/// every fixture boundary. Reproduces tbench's suite-over-suite
/// `resource.mem` growth in-harness.
fn run_mem_suite(fixtures: &[String], grid: GridSize, suites: usize, runs: usize) {
    use noa_pty::{Pty, PtyConfig, PtyEvent};

    let mut script = String::from("i=0; ");
    script.push_str(&format!("while [ $i -lt {suites} ]; do "));
    for f in fixtures {
        script.push_str(&format!(
            "j=0; while [ $j -lt {runs} ]; do cat '{f}'; printf '\\a'; j=$((j+1)); done; "
        ));
    }
    script.push_str("i=$((i+1)); done");
    let cfg = PtyConfig {
        size: grid,
        shell: None,
        command: Some(vec!["/bin/sh".to_string(), "-c".to_string(), script]),
        cwd: None,
        term: "xterm-256color".to_string(),
        login: false,
        shell_integration: false,
    };
    let pty = Pty::spawn(cfg).expect("spawn pty");
    let mut stream = Stream::new();
    let mut term = Terminal::new(grid);
    println!("start: rss {:.1} MB", current_rss() as f64 / 1e6);

    let per_fixture = runs;
    let per_suite = per_fixture * fixtures.len();
    let total = per_suite * suites;
    let mut bels = 0usize;
    let timeout = Duration::from_secs(300);
    loop {
        match pty.event_rx().recv_timeout(timeout) {
            Ok(PtyEvent::Data(chunk)) => {
                let bytes = chunk.as_ref();
                stream.feed(bytes, &mut term);
                for &b in bytes.iter().filter(|&&b| b == 0x07) {
                    let _ = b;
                    bels += 1;
                    if bels.is_multiple_of(per_fixture) {
                        // Model the app's idle trim between fixture batches
                        // (suppressible: tbench's cadence never lets the
                        // 30s-quiescence trim fire).
                        if std::env::var_os("DS_NO_TRIM").is_none() {
                            term.trim_memory();
                        }
                        // Model the app's mid-burst free-page relief
                        // (pressure_relief only, no settle).
                        if std::env::var_os("DS_RELIEF").is_some() {
                            pressure_relief();
                        }
                        let fixture_idx = (bels / per_fixture - 1) % fixtures.len();
                        let suite = (bels - 1) / per_suite + 1;
                        println!(
                            "suite {suite} fixture {}: rss {:.1} MB",
                            fixtures[fixture_idx].rsplit('/').next().unwrap_or("?"),
                            current_rss() as f64 / 1e6
                        );
                    }
                }
                if bels >= total {
                    break;
                }
            }
            Ok(PtyEvent::Exit(_)) | Ok(PtyEvent::Error(_)) => break,
            Err(_) => break,
        }
    }
    term.trim_memory();
    println!("end: rss {:.1} MB", current_rss() as f64 / 1e6);
}

/// SKEPTIC HARNESS (uncommitted): interactive DSR roundtrip under flood.
///
/// A python "app" spawned through the real `noa_pty::Pty` writes a `burst`-byte
/// output burst, then a cursor-position query (`ESC[6n`), then blocks reading
/// its stdin for the CPR reply — recording query->reply wall-clock. Our
/// consumer drives the real pipeline (reader thread -> channel -> parse+apply
/// under the lock) and echoes `take_pending_writes()` back via `pty.writer()`,
/// exactly like io_thread.rs. Models DOOM-fire's "heavy output then a sync
/// query" req/resp pattern; measures whether the reader's coalescing/spin
/// bridge delays a response riding the tail of a burst.
fn run_roundtrip(burst: usize, iters: usize, grid: GridSize) {
    use noa_pty::{Pty, PtyConfig, PtyEvent};

    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let tmpdir = tmpdir.trim_end_matches('/').to_string();
    let script = format!("{tmpdir}/rt_app_{}.py", std::process::id());
    let result = format!("{tmpdir}/rt_res_{}.txt", std::process::id());
    let py = r#"
import os, sys, time
resfile = sys.argv[1]; burst = int(sys.argv[2]); iters = int(sys.argv[3])
line = b"DOOM fire req/resp flood abcdefghijklmnopqrstuvwxyz 0123456789 the quick brown fox\r\n"
buf = bytearray()
while len(buf) < burst:
    buf += line
buf = bytes(buf[:burst])
rtts = []
for i in range(iters + 8):            # first 8 are warmup, dropped
    off = 0
    while off < len(buf):
        off += os.write(1, buf[off:])
    t0 = time.perf_counter_ns()
    os.write(1, b"\x1b[6n")
    resp = b""
    while b"R" not in resp:
        resp += os.read(0, 32)
    t1 = time.perf_counter_ns()
    if i >= 8:
        rtts.append(t1 - t0)
with open(resfile, "w") as f:
    f.write("\n".join(str(x) for x in rtts))
"#;
    std::fs::write(&script, py).expect("write app script");

    let cfg = PtyConfig {
        size: grid,
        shell: None,
        command: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("stty raw -echo 2>/dev/null; exec python3 {script} {result} {burst} {iters}"),
        ]),
        cwd: None,
        term: "xterm-256color".to_string(),
        login: false,
        shell_integration: false,
    };
    let pty = Pty::spawn(cfg).expect("spawn pty");
    let writer = pty.writer();
    let mut stream = Stream::new();
    let mut term = Terminal::new(grid);

    let timeout = Duration::from_secs(120);
    loop {
        match pty.event_rx().recv_timeout(timeout) {
            Ok(PtyEvent::Data(chunk)) => {
                stream.feed(chunk.as_ref(), &mut term);
                let reply = term.take_pending_writes();
                if !reply.is_empty() {
                    let _ = writer.write(&reply);
                }
            }
            Ok(PtyEvent::Exit(_)) | Ok(PtyEvent::Error(_)) => break,
            Err(_) => break,
        }
    }
    let text = std::fs::read_to_string(&result).unwrap_or_default();
    let _ = std::fs::remove_file(&script);
    let _ = std::fs::remove_file(&result);
    let mut ns: Vec<u64> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
    if ns.is_empty() {
        println!("roundtrip burst={burst}: NO SAMPLES (app failed)");
        return;
    }
    ns.sort_unstable();
    let pct = |p: f64| ns[((ns.len() as f64 * p) as usize).min(ns.len() - 1)];
    let mean = ns.iter().sum::<u64>() as f64 / ns.len() as f64;
    println!(
        "roundtrip burst={:>7} n={:>4}: mean {:>7.1}us p50 {:>7.1} p90 {:>7.1} p99 {:>7.1} max {:>7.1} us",
        burst,
        ns.len(),
        mean / 1000.0,
        pct(0.50) as f64 / 1000.0,
        pct(0.90) as f64 / 1000.0,
        pct(0.99) as f64 / 1000.0,
        *ns.last().unwrap() as f64 / 1000.0,
    );
}

fn write_workload_file(path: &str, tmpl: &[u8], total: usize) {
    use std::io::Write as _;
    let f = std::fs::File::create(path).expect("create workload file");
    let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
    let mut written = 0usize;
    while written < total {
        let end = (written + tmpl.len()).min(total);
        w.write_all(&tmpl[..end - written]).expect("write");
        written = end;
    }
    w.flush().expect("flush");
}

// ── driver ─────────────────────────────────────────────────────────────

fn mbps(total: usize, d: Duration) -> f64 {
    (total as f64 / (1024.0 * 1024.0)) / d.as_secs_f64()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn measure<F: FnMut() -> Duration>(reps: usize, total: usize, mut run: F) -> f64 {
    let _ = run(); // warm-up
    median((0..reps).map(|_| mbps(total, run())).collect())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str, default: usize| -> usize {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let has = |name: &str| args.iter().any(|a| a == name);
    let str_flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    // Profiling mode: loop one workload/stage in a tight single-thread loop so
    // an external sampler can take a function-level apply-stage profile.
    //   drain-staircase --proc-profile --stage s4 --workload plain --secs 30
    if has("--proc-profile") {
        let cols = flag("--cols", 80) as u16;
        let rows = flag("--rows", 24) as u16;
        let grid = GridSize::new(cols, rows);
        let secs = flag("--secs", 30) as u64;
        let stage = match str_flag("--stage").as_deref() {
            Some("s3") => Stage::S3ApplyNoSb,
            _ => Stage::S4ApplySb,
        };
        let w = match str_flag("--workload").as_deref() {
            Some("ansi") => Workload::Ansi,
            Some("scroll") => Workload::Scroll,
            _ => Workload::Plain,
        };
        run_proc_profile(stage, w, secs, grid);
        return;
    }

    // Single authoritative real-lens run (task-3 iteration + external
    // sampling window):
    //   drain-staircase --real-one --stage s4 --workload plain --mb 512
    if has("--real-one") {
        let cols = flag("--cols", 80) as u16;
        let rows = flag("--rows", 24) as u16;
        let grid = GridSize::new(cols, rows);
        let mb = flag("--mb", 256);
        let total = mb * 1024 * 1024;
        let stage = match str_flag("--stage").as_deref() {
            Some("s1") => Stage::S1Discard,
            Some("s2") => Stage::S2Parse,
            Some("s3") => Stage::S3ApplyNoSb,
            _ => Stage::S4ApplySb,
        };
        let w = match str_flag("--workload").as_deref() {
            Some("ansi") => Workload::Ansi,
            Some("scroll") => Workload::Scroll,
            _ => Workload::Plain,
        };
        let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        let path = format!(
            "{}/drain_one_{}.dat",
            tmpdir.trim_end_matches('/'),
            w.name()
        );
        let tmpl = template(w);
        write_workload_file(&path, &tmpl, total);
        eprintln!(
            "real-one: {} {} {}x{} {} MiB — pid {}",
            stage_name(stage),
            w.name(),
            cols,
            rows,
            mb,
            std::process::id()
        );
        let dur = run_real(stage, &path, total, grid, false);
        let _ = std::fs::remove_file(&path);
        println!(
            "real-one {} {}: {:.1} MB/s ({:.2}s)",
            stage_name(stage),
            w.name(),
            total as f64 / dur.as_secs_f64() / 1e6,
            dur.as_secs_f64()
        );
        return;
    }

    // tbench-faithful mode (skeptic-oracle attack #2): real fixture bytes,
    // real ONLCR, one warm session, 20 sequential runs.
    //   drain-staircase --tbench-faithful --warmup 3 --runs 20
    if has("--tbench-faithful") {
        let cols = flag("--cols", 80) as u16;
        let rows = flag("--rows", 24) as u16;
        let grid = GridSize::new(cols, rows);
        let warmup = flag("--warmup", 3);
        let measured = flag("--runs", 20);
        let fixture = str_flag("--fixture").unwrap_or_else(|| {
            "/Users/simota/repos/github.com/TerminalBench/fixtures/large-output-plain.txt"
                .to_string()
        });
        let fixture_len = std::fs::metadata(&fixture).expect("stat fixture").len() as usize;
        eprintln!(
            "tbench-faithful: fixture={fixture} ({fixture_len}B) grid={cols}x{rows} warmup={warmup} runs={measured} — pid {}",
            std::process::id()
        );
        let raw_tty = has("--raw-tty");
        let stage = match str_flag("--stage").as_deref() {
            Some("s1") => Stage::S1Discard,
            Some("s2") => Stage::S2Parse,
            Some("s3") => Stage::S3ApplyNoSb,
            _ => Stage::S4ApplySb,
        };
        let durs = run_tbench_faithful(&fixture, grid, warmup, measured, raw_tty, stage);
        let mbps_vals: Vec<f64> = durs.iter().map(|d| mbps(fixture_len, *d)).collect();
        for (i, (m, d)) in mbps_vals.iter().zip(durs.iter()).enumerate() {
            println!("run {:2}: {:.1} MB/s ({:.4}s)", i + 1, m, d.as_secs_f64());
        }
        let med = median(mbps_vals.clone());
        let mean: f64 = mbps_vals.iter().sum::<f64>() / mbps_vals.len() as f64;
        let variance: f64 =
            mbps_vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / mbps_vals.len() as f64;
        let stddev = variance.sqrt();
        println!(
            "\ntbench-faithful {} plain: median {:.1} MB/s, mean {:.1}, stddev {:.2}, n={}",
            stage_name(stage),
            med,
            mean,
            stddev,
            mbps_vals.len()
        );
        return;
    }

    // mem-suite mode (bonus-cycle memory-retention oracle):
    //   drain-staircase --mem-suite --suites 3 --runs 20
    if has("--mem-suite") {
        let cols = flag("--cols", 80) as u16;
        let rows = flag("--rows", 24) as u16;
        let grid = GridSize::new(cols, rows);
        let suites = flag("--suites", 3);
        let runs = flag("--runs", 20);
        let base = "/Users/simota/repos/github.com/TerminalBench/fixtures";
        let fixtures: Vec<String> = [
            "large-output-plain.txt",
            "large-output-ansi.txt",
            "large-output-cjk.txt",
        ]
        .iter()
        .map(|f| format!("{base}/{f}"))
        .collect();
        run_mem_suite(&fixtures, grid, suites, runs);
        return;
    }

    // roundtrip mode (skeptic-defense attack #3): interactive DSR reply
    // latency riding the tail of an output burst.
    //   drain-staircase --roundtrip --burst 262144 --iters 200 --cols 140 --rows 40
    if has("--roundtrip") {
        let cols = flag("--cols", 140) as u16;
        let rows = flag("--rows", 40) as u16;
        let grid = GridSize::new(cols, rows);
        let iters = flag("--iters", 200);
        for burst in [4096usize, 65536, 262144, 1048576] {
            if has("--burst") {
                run_roundtrip(flag("--burst", 262144), iters, grid);
                break;
            }
            run_roundtrip(burst, iters, grid);
        }
        return;
    }

    let mb = flag("--mb", 256);
    let reps = flag("--reps", 3);
    let cols = flag("--cols", 80) as u16;
    let rows = flag("--rows", 24) as u16;
    let contend = has("--contend");
    let grid = GridSize::new(cols, rows);
    let total = mb * 1024 * 1024;
    let warm = total.min(16 * 1024 * 1024);

    println!(
        "drain-staircase: {mb} MiB/run, reps={reps} (median-of), grid={cols}x{rows}, Apple M4"
    );
    println!("numbers = MB/s of producer PTY write-completion wall-clock\n");

    // S0 reference.
    let cap = run_s0_buffer_probe(grid);
    println!(
        "S0  no reader: producer stalls after {} bytes buffered (kernel pty buffer) -> ~0 MB/s\n",
        cap
    );

    // Naive one-thread blocking read floor (illustrative).
    {
        print!("naive 1-thread blocking read+discard (floor): ");
        let mut cells = Vec::new();
        for w in Workload::all() {
            let tmpl = template(w);
            cells.push(measure(reps, total, || {
                run_naive_discard(&tmpl, total, grid)
            }));
        }
        println!(
            "plain {:.0}  ansi {:.0}  scroll {:.0} MB/s\n",
            cells[0], cells[1], cells[2]
        );
        let _ = warm;
    }

    // proc: pure processing ceiling.
    println!("== proc: pure in-memory processing ceiling (no pty) ==");
    println!(
        "{:<18} {:>10} {:>10} {:>10}",
        "stage", "plain", "ansi", "scroll"
    );
    for stage in [Stage::S2Parse, Stage::S3ApplyNoSb, Stage::S4ApplySb] {
        let mut cells = Vec::new();
        for w in Workload::all() {
            let tmpl = template(w);
            cells.push(measure(reps, total, || run_proc(stage, &tmpl, total, grid)));
        }
        println!(
            "{:<18} {:>10.1} {:>10.1} {:>10.1}",
            stage_name(stage),
            cells[0],
            cells[1],
            cells[2]
        );
    }

    // real: production pipeline drain (authoritative).
    println!("\n== real: production noa_pty pipeline (reader thread + locked consumer) ==");
    println!(
        "{:<18} {:>10} {:>10} {:>10}",
        "stage", "plain", "ansi", "scroll"
    );
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let tmpdir = tmpdir.trim_end_matches('/').to_string();
    for stage in [
        Stage::S1Discard,
        Stage::S2Parse,
        Stage::S3ApplyNoSb,
        Stage::S4ApplySb,
    ] {
        let mut cells = Vec::new();
        for w in Workload::all() {
            let tmpl = template(w);
            let path = format!("{}/drain_{}.dat", tmpdir, w.name());
            write_workload_file(&path, &tmpl, total);
            cells.push(measure(reps, total, || {
                run_real(stage, &path, total, grid, false)
            }));
            let _ = std::fs::remove_file(&path);
        }
        println!(
            "{:<18} {:>10.1} {:>10.1} {:>10.1}",
            stage_name(stage),
            cells[0],
            cells[1],
            cells[2]
        );
    }

    if contend {
        println!("\n== real + ~120Hz snapshot lock contender (S4) ==");
        println!(
            "{:<18} {:>10} {:>10} {:>10}",
            "stage", "plain", "ansi", "scroll"
        );
        let mut cells = Vec::new();
        for w in Workload::all() {
            let tmpl = template(w);
            let path = format!("{}/drain_{}.dat", tmpdir, w.name());
            write_workload_file(&path, &tmpl, total);
            cells.push(measure(reps, total, || {
                run_real(Stage::S4ApplySb, &path, total, grid, true)
            }));
            let _ = std::fs::remove_file(&path);
        }
        println!(
            "{:<18} {:>10.1} {:>10.1} {:>10.1}",
            "S4 +contender", cells[0], cells[1], cells[2]
        );
    }
}
