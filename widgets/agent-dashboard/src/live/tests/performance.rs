use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

// Build the counting run separately so timings use the system allocator
// without atomic increments. Run probes alone with --test-threads=1.
struct Allocator;

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

fn count(bytes: usize) {
    if option_env!("DASHBOARD_COUNT_ALLOC").is_some() {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(bytes as u64, Relaxed);
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn wanted(name: &str) -> bool {
    std::env::var("DASHBOARD_PROBE").map_or(true, |probe| probe == name)
}

fn measure(name: &str, panes: usize, batch: usize, mut work: impl FnMut()) {
    if !wanted(name) {
        return;
    }
    work();
    let mut samples = Vec::with_capacity(101);
    let calls = CALLS.load(Relaxed);
    let bytes = BYTES.load(Relaxed);
    for _ in 0..101 {
        let start = Instant::now();
        for _ in 0..batch {
            work();
        }
        samples.push(start.elapsed().as_nanos() as f64 / batch as f64);
    }
    let units = (samples.len() * batch) as f64;
    let calls = (CALLS.load(Relaxed) - calls) as f64 / units;
    let bytes = (BYTES.load(Relaxed) - bytes) as f64 / units;
    samples.sort_by(f64::total_cmp);
    println!(
        "probe {name} {panes} {:.1} {:.1} {:.1} {calls:.2} {bytes:.2}",
        samples[50], samples[95], samples[99]
    );
}

fn server(n: usize, clock: &TestClock) -> ScriptedNoa {
    let mut server = ScriptedNoa::showing("instance", "compiling", clock);
    for pane in 3..n + 2 {
        server
            .panels
            .push(running_agent(&pane.to_string(), "1", "building"));
    }
    server
}

#[test]
#[ignore = "manual fake-clock IPC traffic measurement"]
fn dashboard_request_probe() {
    for n in [1, 20, 100] {
        for (step_ms, watched) in [(1000, 0), (100, 0), (100, n.min(2))] {
            let clock = TestClock::new();
            let (mut feed, _snapshots, _commands) = held_feed(&clock);
            let mut server = server(n, &clock);
            if step_ms == 100 {
                server.push_panel = Some(server.panels[1].clone());
            }
            shows(&mut feed, server.instance());
            let panes: Vec<_> = (2..2 + watched as u64).collect();
            for &pane in &panes {
                server.screens.insert(pane, "compiling".to_owned());
            }
            feed.watch(panes);
            server.waits = 60_000 / step_ms;
            server.wait_takes = Duration::from_millis(step_ms as u64);
            assert!(matches!(feed.follow(&mut server), Err(Stop::Lost(_))));
            println!(
                "requests {n} {step_ms} {watched} {} {} {}",
                server.subscriptions,
                server.lists,
                server.fetched.len()
            );
        }
    }
}

#[test]
#[ignore = "manual release-mode model and frame measurement"]
fn dashboard_update_probe() {
    for n in [1, 20, 100] {
        let clock = TestClock::new();
        let (mut feed, snapshots, _commands) = held_feed(&clock);
        let server = server(n, &clock);
        feed.tracker.replace(server.panels.clone(), unix_now());
        let fleet = feed.tracker.fleet(unix_now(), None);
        let mut later = fleet.clone();
        later.workers.last_mut().unwrap().reports += 1;
        measure("structure", n, 64, || {
            black_box(black_box(&fleet).same_structure(black_box(&later)));
        });
        let mut sent = None;
        feed.send_if_due(&server, &mut sent).ok().unwrap();
        let snapshot = snapshots.recv().unwrap();
        measure("feed-check", n, 8, || {
            feed.send_if_due(&server, &mut sent).ok().unwrap();
        });
        measure("feed-pulse", n, 8, || {
            clock.advance(PULSE_EVERY);
            feed.send_if_due(&server, &mut sent).ok().unwrap();
            black_box(snapshots.recv().unwrap());
        });
        let mut panel = server.panels[1].clone();
        measure("feed-change", n, 8, || {
            panel.attention = !panel.attention;
            feed.tracker.merge(vec![panel.clone()], unix_now());
            assert!(feed.send_if_due(&server, &mut sent).is_ok());
            black_box(snapshots.recv().unwrap());
        });
        if !wanted("settled-frame") && !wanted("pulse-frame") {
            continue;
        }
        let (channel, publish, _notices, _watch) = Channel::detached();
        let mut board = Dashboard::live(Grouping::Orchestrator, channel);
        if let Ok(query) = std::env::var("DASHBOARD_FILTER") {
            board = board.with_filter(query);
        }
        let mut text = TextStore::new(Default::default());
        let size = Size::new(1440.0, 900.0);
        publish.send(snapshot.clone()).unwrap();
        board.before_frame(&mut text);
        board.prepare(size, &mut text);
        if std::env::var_os("DASHBOARD_SELECT").is_some() {
            board.dispatch(
                Event::KeyDown {
                    key: Key::Char('j'),
                    modifiers: Modifiers::default(),
                },
                &mut text,
            );
            board.prepare(size, &mut text);
        }
        measure("settled-frame", n, 4, || {
            board.before_frame(&mut text);
            black_box(board.prepare(size, &mut text));
        });
        measure("pulse-frame", n, 1, || {
            publish.send(snapshot.clone()).unwrap();
            board.before_frame(&mut text);
            black_box(board.prepare(size, &mut text));
        });
    }
}
