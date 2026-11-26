//! AC-6 measurement (`docs/specs/board-timeline.md`): a board of 10 000
//! posts (bodies 0.5–8 KiB, seeded), then the first `list` page, a past
//! page, a `query` that hits ~1% of the posts, one that hits none (the
//! full body scan of D12/R1), a cold replay, and peak RSS. A measurement,
//! not a check: it asserts nothing and is `#[ignore]`d so `cargo test`
//! does not build the board. Run:
//!
//! ```text
//! cargo test --offline -p noa-app --release timeline::bench -- --ignored --nocapture
//! ```

use std::time::Instant;

use noa_ipc::timeline::TimelineCall;
use serde_json::{Value, json};

use super::store::{RealIo, Store, StoreLimits};

const POSTS: usize = 10_000;
const HIT_EVERY: usize = 100;
const NEEDLE: &str = "zebraquark";

/// xorshift64*: a fixed sequence, so every run builds the same board.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn body(rng: &mut Rng, hit: bool) -> String {
    const WORDS: [&str; 12] = [
        "parser", "grid", "render", "scroll", "pane", "board", "worker", "review", "verify",
        "atlas", "glyph", "reflow",
    ];
    let len = 512 + rng.below(8 * 1024 - 512) as usize;
    let mut out = String::with_capacity(len + 16);
    if hit {
        out.push_str(NEEDLE);
        out.push(' ');
    }
    while out.len() < len {
        out.push_str(WORDS[rng.below(WORDS.len() as u64) as usize]);
        out.push(' ');
    }
    out.truncate(len);
    out
}

/// `ru_maxrss` in bytes (macOS reports bytes, Linux KiB).
fn peak_rss_bytes() -> u64 {
    // SAFETY: `getrusage` fills the zeroed struct it is given.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let raw = u64::try_from(usage.ru_maxrss).unwrap_or(0);
    if cfg!(target_os = "macos") {
        raw
    } else {
        raw * 1024
    }
}

fn run(store: &mut Store, method: &str, params: Value) -> Value {
    let call = TimelineCall::parse(method, params).unwrap().unwrap();
    store.handle(call, &[]).unwrap()
}

fn timed(store: &mut Store, method: &str, params: Value) -> (f64, Value) {
    let start = Instant::now();
    let result = run(store, method, params);
    (start.elapsed().as_secs_f64() * 1000.0, result)
}

#[test]
#[ignore = "AC-6 measurement: builds a 10 000-post board (seconds); run with --ignored"]
fn ac6_ten_thousand_posts() {
    let root = std::env::temp_dir()
        .join(format!("noa-timeline-bench-{}", std::process::id()))
        .join("boards");
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
    let limits = StoreLimits {
        max_board_bytes: u64::MAX,
    };
    let rss_start = peak_rss_bytes();
    let mut store = Store::open(root.clone(), "bench".into(), limits, Box::new(RealIo)).unwrap();
    let created = run(
        &mut store,
        "noa.timeline.registerRun",
        json!({ "requestId": "bench", "publishPolicy": { "owner": "runner", "producerId": "r" } }),
    );
    let board = created["boardId"].as_str().unwrap().to_owned();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let build = Instant::now();
    for i in 0..POSTS {
        let post = json!({
            "schemaVersion": 1, "kind": "milestone", "outcome": "succeeded",
            "title": format!("post {i}"), "summary": format!("summary of post {i}"),
            "bodyMarkdown": body(&mut rng, i % HIT_EVERY == 0),
            "sourceRef": { "namespace": "manual", "sessionId": "bench", "eventId": format!("e{i}") },
        });
        run(
            &mut store,
            "noa.timeline.post",
            json!({ "boardId": board, "runId": null, "producerId": "cli",
                    "requestId": format!("r{i}"), "post": post }),
        );
    }
    let build_ms = build.elapsed().as_secs_f64() * 1000.0;
    let rss_built = peak_rss_bytes();

    let (first_ms, _) = timed(&mut store, "noa.timeline.list", json!({ "boardId": board }));
    let (past_ms, _) = timed(
        &mut store,
        "noa.timeline.list",
        json!({ "boardId": board, "beforeSeq": "5000" }),
    );
    let (hit_ms, hits) = timed(
        &mut store,
        "noa.timeline.list",
        json!({ "boardId": board, "query": NEEDLE, "limit": 50 }),
    );
    let (miss_ms, _) = timed(
        &mut store,
        "noa.timeline.list",
        json!({ "boardId": board, "query": "nowhere-in-any-post" }),
    );
    let (changes_ms, _) = timed(
        &mut store,
        "noa.timeline.changes",
        json!({ "boardId": board, "afterSeq": "0", "limit": 200 }),
    );
    drop(store);

    let replay = Instant::now();
    let mut store = Store::open(root.clone(), "bench-2".into(), limits, Box::new(RealIo)).unwrap();
    let replay_ms = replay.elapsed().as_secs_f64() * 1000.0;
    let rss_replayed = peak_rss_bytes();
    let info = run(&mut store, "noa.timeline.boards", json!({}));
    let bytes = info["boards"][0]["bytes"].as_u64().unwrap_or(0);

    println!(
        "{}",
        json!({
            "bench": "ac6_ten_thousand_posts",
            "posts": POSTS,
            "fileBytes": bytes,
            "buildMs": round(build_ms),
            "firstListMs": round(first_ms),
            "pastPageMs": round(past_ms),
            "queryHitMs": round(hit_ms),
            "queryHitCount": hits["posts"].as_array().map_or(0, Vec::len),
            "queryMissFullScanMs": round(miss_ms),
            "changesPageMs": round(changes_ms),
            "coldReplayMs": round(replay_ms),
            "peakRssStartBytes": rss_start,
            "peakRssAfterBuildBytes": rss_built,
            "peakRssAfterReplayBytes": rss_replayed,
        })
    );
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

fn round(ms: f64) -> f64 {
    (ms * 100.0).round() / 100.0
}
