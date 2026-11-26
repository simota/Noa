//! Store and service tests (`docs/specs/board-timeline.md` AC-1, AC-4):
//! the store driven directly on a temp root, the service through its
//! handle.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use noa_ipc::timeline::{
    BoardsResult, ContextResult, GetResult, MAX_BODY_BYTES, PageResult, PostResult,
    RegisterRunResult, RunInfo, TimelineCall, TimelineError, TimelineErrorKind,
};
use noa_ipc::{Panel, Seq};
use serde_json::{Value, json};

use super::service;
use super::store::{RESULT_BUDGET, RealIo, Store, StoreIo, StoreLimits};

const INSTANCE: &str = "instance-a";

static DIRS: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "noa-timeline-{tag}-{}-{}",
        std::process::id(),
        DIRS.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    dir.join("boards")
}

fn open_as(root: &Path, instance: &str) -> Store {
    Store::open(
        root.to_path_buf(),
        instance.to_owned(),
        StoreLimits::default(),
        Box::new(RealIo),
    )
    .unwrap()
}

fn open(root: &Path) -> Store {
    open_as(root, INSTANCE)
}

fn panel(pane: u64, surface: &str) -> Panel {
    serde_json::from_value(json!({
        "agent": { "kind": "claude", "seq": 0 },
        "attention": false, "busy": false, "cwd": "/w", "name": "zsh",
        "paneId": pane.to_string(), "preview": [], "surfaceId": surface,
        "windowGroupId": "1", "windowId": "1",
    }))
    .unwrap()
}

fn panels(panes: &[u64]) -> Vec<Panel> {
    panes.iter().map(|&p| panel(p, &format!("s{p}"))).collect()
}

fn call(
    store: &mut Store,
    method: &str,
    params: Value,
    panels: &[Panel],
) -> Result<Value, TimelineError> {
    let call = TimelineCall::parse(method, params)?.expect("a timeline method");
    store.handle(call, panels)
}

fn ok<T: serde::de::DeserializeOwned>(result: Result<Value, TimelineError>) -> T {
    serde_json::from_value(result.expect("call succeeds")).unwrap()
}

fn kind(result: Result<Value, TimelineError>) -> TimelineErrorKind {
    result.expect_err("call refused").kind
}

/// A board with one runner-owned run (producer `runner:t`).
fn board(store: &mut Store, request: &str) -> RegisterRunResult {
    ok(call(
        store,
        "noa.timeline.registerRun",
        json!({
            "requestId": request,
            "publishPolicy": { "owner": "runner", "producerId": "runner:t" },
        }),
        &[],
    ))
}

fn register(
    store: &mut Store,
    request: &str,
    orchestrator: Option<u64>,
    worker: Option<u64>,
    extra: Value,
    panels: &[Panel],
) -> Result<Value, TimelineError> {
    let mut params = json!({
        "requestId": request,
        "publishPolicy": { "owner": "worker" },
    });
    if let Some(pane) = orchestrator {
        params["orchestratorPaneId"] = json!(pane.to_string());
    }
    if let Some(pane) = worker {
        params["workerPaneId"] = json!(pane.to_string());
    }
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        params[k] = v;
    }
    call(store, "noa.timeline.registerRun", params, panels)
}

fn input(title: &str, event: &str) -> Value {
    json!({
        "schemaVersion": 1, "kind": "milestone", "outcome": "succeeded",
        "title": title, "summary": format!("summary of {title}"),
        "sourceRef": { "namespace": "manual", "sessionId": "s", "eventId": event },
    })
}

/// An unregistered (`manual`) post.
fn manual(board: &str, producer: &str, request: &str, post: Value) -> Value {
    json!({
        "boardId": board, "runId": null, "producerId": producer,
        "requestId": request, "post": post,
    })
}

fn post(store: &mut Store, params: Value) -> Result<Value, TimelineError> {
    call(store, "noa.timeline.post", params, &[])
}

fn post_n(store: &mut Store, board: &str, n: usize) -> Vec<PostResult> {
    (0..n)
        .map(|i| {
            ok(post(
                store,
                manual(
                    board,
                    "cli",
                    &format!("r{i}"),
                    input(&format!("t{i}"), &format!("e{i}")),
                ),
            ))
        })
        .collect()
}

fn list(store: &mut Store, params: Value) -> PageResult {
    ok(call(store, "noa.timeline.list", params, &[]))
}

fn changes(store: &mut Store, board: &str, after: u64, limit: usize) -> PageResult {
    ok(call(
        store,
        "noa.timeline.changes",
        json!({ "boardId": board, "afterSeq": after.to_string(), "limit": limit }),
        &[],
    ))
}

fn seqs(page: &PageResult) -> Vec<u64> {
    page.posts.iter().map(|p| p.seq.0).collect()
}

fn context(store: &mut Store, params: Value, panels: &[Panel]) -> ContextResult {
    ok(call(store, "noa.timeline.context", params, panels))
}

fn posts_file(root: &Path, board: &str) -> PathBuf {
    root.join(board).join("posts.jsonl")
}

// ---------------------------------------------------------------------------
// AC-1

#[test]
fn ac1_replay_after_restart_keeps_seq_order() {
    let root = temp_root("replay");
    let b = {
        let mut store = open(&root);
        let b = board(&mut store, "b").board_id;
        post_n(&mut store, &b, 5);
        b
    };
    let mut store = open(&root);
    let page = list(&mut store, json!({ "boardId": b, "limit": 50 }));
    assert_eq!(seqs(&page), [5, 4, 3, 2, 1]);
    assert_eq!(page.posts[0].title, "t4");
    assert_eq!(page.head_seq, Seq(5));
    assert_eq!(seqs(&changes(&mut store, &b, 0, 200)), [1, 2, 3, 4, 5]);
    let next: PostResult = ok(post(&mut store, manual(&b, "cli", "r5", input("t5", "e5"))));
    assert_eq!(next.seq, Seq(6));
}

#[test]
fn ac1_same_key_same_payload_returns_same_post() {
    let root = temp_root("same-key");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    let first: PostResult = ok(post(&mut store, manual(&b, "cli", "r1", input("t", "e1"))));
    assert!(!first.duplicate);
    let again: PostResult = ok(post(&mut store, manual(&b, "cli", "r1", input("t", "e1"))));
    assert!(again.duplicate);
    assert_eq!(
        (&again.post_id, again.seq, &again.created_at),
        (&first.post_id, first.seq, &first.created_at)
    );
    assert_eq!(list(&mut store, json!({ "boardId": b })).head_seq, Seq(1));
    // Also after a restart (the indexes are rebuilt from the file).
    drop(store);
    let mut store = open(&root);
    let replayed: PostResult = ok(post(&mut store, manual(&b, "cli", "r1", input("t", "e1"))));
    assert!(replayed.duplicate);
    assert_eq!(replayed.post_id, first.post_id);
}

#[test]
fn ac1_key_order_and_defaults_do_not_matter() {
    let root = temp_root("canon");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    let terse = format!(
        r#"{{"requestId":"r1","producerId":"cli","boardId":"{b}","post":{{
            "title":"t","summary":"s","outcome":"succeeded","kind":"milestone","schemaVersion":1,
            "openIssues":["fix x"],
            "sourceRef":{{"sessionId":"s","namespace":"manual","eventId":"e1"}}}}}}"#
    );
    let spelled = format!(
        r#"{{"boardId":"{b}","runId":null,"producerId":"cli","requestId":"r1","post":{{
            "schemaVersion":1,"kind":"milestone","outcome":"succeeded","title":"t","summary":"s",
            "bodyMarkdown":"","occurredAt":null,"author":null,"taskRef":null,"retryOfRunId":null,
            "artifacts":[],"checks":[],"openIssues":[{{"ref":null,"text":"fix x"}}],
            "nextActions":[],"basedOnPostIds":[],"supersedesPostId":null,
            "sourceRef":{{"eventId":"e1","locator":null,"namespace":"manual","runId":null,
                          "sessionId":"s"}}}}}}"#
    );
    let first: PostResult = ok(post(&mut store, serde_json::from_str(&terse).unwrap()));
    let second: PostResult = ok(post(&mut store, serde_json::from_str(&spelled).unwrap()));
    assert!(second.duplicate);
    assert_eq!(second.post_id, first.post_id);
}

#[test]
fn ac1_same_key_other_payload_conflicts_and_store_unchanged() {
    let root = temp_root("key-conflict");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    let first: PostResult = ok(post(&mut store, manual(&b, "cli", "r1", input("t", "e1"))));
    let before = fs::read(posts_file(&root, &b)).unwrap();
    let err = post(
        &mut store,
        manual(&b, "cli", "r1", input("other title", "e1")),
    )
    .unwrap_err();
    assert_eq!(err.kind, TimelineErrorKind::Conflict);
    assert_eq!(
        err.existing_post_id.as_deref(),
        Some(first.post_id.as_str())
    );
    assert_eq!(err.data()["existingPostId"], json!(first.post_id));
    assert_eq!(fs::read(posts_file(&root, &b)).unwrap(), before);
    assert_eq!(list(&mut store, json!({ "boardId": b })).head_seq, Seq(1));
}

#[test]
fn ac1_same_source_other_request_collapses() {
    let root = temp_root("source-collapse");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    let first: PostResult = ok(post(&mut store, manual(&b, "cli", "r1", input("t", "e1"))));
    // Another request id, even relayed by another producer: one source event.
    let relayed: PostResult = ok(post(
        &mut store,
        manual(&b, "relay", "r2", input("t", "e1")),
    ));
    assert!(relayed.duplicate);
    assert_eq!(relayed.post_id, first.post_id);
    assert_eq!(list(&mut store, json!({ "boardId": b })).head_seq, Seq(1));
}

#[test]
fn ac1_same_source_other_body_conflicts() {
    let root = temp_root("source-conflict");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    let first: PostResult = ok(post(&mut store, manual(&b, "cli", "r1", input("t", "e1"))));
    let mut changed = input("t", "e1");
    changed["bodyMarkdown"] = json!("rewritten");
    let err = post(&mut store, manual(&b, "cli", "r2", changed)).unwrap_err();
    assert_eq!(err.kind, TimelineErrorKind::Conflict);
    assert_eq!(err.existing_post_id, Some(first.post_id));
    assert_eq!(list(&mut store, json!({ "boardId": b })).head_seq, Seq(1));
}

#[test]
fn ac1_second_initial_summary_conflicts() {
    let root = temp_root("summary");
    let mut store = open(&root);
    let run = board(&mut store, "b");
    let summary = |event: &str, supersedes: Option<&str>| {
        let mut post = input("done", event);
        post["kind"] = json!("summary");
        post["sourceRef"]["namespace"] = json!("tasks");
        post["supersedesPostId"] = json!(supersedes);
        json!({
            "boardId": run.board_id, "runId": run.run_id, "producerId": "runner:t",
            "requestId": format!("req-{event}"), "post": post,
        })
    };
    let first: PostResult = ok(post(&mut store, summary("e1", None)));
    let err = post(&mut store, summary("e2", None)).unwrap_err();
    assert_eq!(err.kind, TimelineErrorKind::Conflict);
    assert_eq!(
        err.existing_post_id.as_deref(),
        Some(first.post_id.as_str())
    );
    // A correction names the first summary and has its own event id.
    let correction: PostResult = ok(post(&mut store, summary("e3", Some(&first.post_id))));
    assert_eq!(correction.seq, Seq(2));
    let got: GetResult = ok(call(
        &mut store,
        "noa.timeline.get",
        json!({ "boardId": run.board_id, "postId": first.post_id }),
        &[],
    ));
    assert_eq!(got.superseded_by, std::slice::from_ref(&correction.post_id));
    let ctx = context(&mut store, json!({ "boardId": run.board_id }), &[]);
    assert!(ctx.workers.is_empty());
    // The runner-owned run has no worker; its summary is still recorded.
    drop(store);
    let mut store = open(&root);
    let err = post(&mut store, summary("e4", None)).unwrap_err();
    assert_eq!(err.existing_post_id, Some(first.post_id));
}

#[test]
fn ac1_concurrent_producers_lose_nothing() {
    let root = temp_root("concurrent");
    let handle = service::start_with(
        root.clone(),
        INSTANCE.to_owned(),
        StoreLimits::default(),
        Box::new(RealIo),
        |_| {},
    )
    .unwrap();
    let run = |call: TimelineCall| handle.call(call, Arc::new(Vec::new()));
    let created: RegisterRunResult = ok(run(TimelineCall::parse(
        "noa.timeline.registerRun",
        json!({ "requestId": "b", "publishPolicy": { "owner": "runner", "producerId": "r" } }),
    )
    .unwrap()
    .unwrap()));
    let b = created.board_id;
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let handle = handle.clone();
            let b = b.clone();
            std::thread::spawn(move || {
                (0..50)
                    .map(|i| {
                        let params = manual(
                            &b,
                            &format!("p{t}"),
                            &format!("r{i}"),
                            input(&format!("t{t}-{i}"), &format!("e{t}-{i}")),
                        );
                        let call = TimelineCall::parse("noa.timeline.post", params)
                            .unwrap()
                            .unwrap();
                        ok::<PostResult>(handle.call(call, Arc::new(Vec::new())))
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let results: Vec<PostResult> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    assert_eq!(results.len(), 400);
    assert!(results.iter().all(|r| !r.duplicate));
    let mut seqs: Vec<u64> = results.iter().map(|r| r.seq.0).collect();
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=400).collect::<Vec<_>>());
    let ids: std::collections::HashSet<_> = results.iter().map(|r| &r.post_id).collect();
    assert_eq!(ids.len(), 400);
    // And the file replays to the same 400.
    drop(handle);
    std::thread::sleep(Duration::from_millis(50));
    let mut store = open_as(&root, "instance-b");
    let mut all = Vec::new();
    let mut after = 0;
    loop {
        let page = changes(&mut store, &b, after, 200);
        all.extend(seqs_of(&page));
        after = page.next_after_seq.unwrap().0;
        if !page.has_more {
            break;
        }
    }
    assert_eq!(all, (1..=400).collect::<Vec<_>>());
}

fn seqs_of(page: &PageResult) -> Vec<u64> {
    seqs(page)
}

#[test]
fn ac1_torn_tail_quarantined_and_resend_succeeds() {
    let root = temp_root("torn");
    let b = {
        let mut store = open(&root);
        let b = board(&mut store, "b").board_id;
        post_n(&mut store, &b, 2);
        b
    };
    let torn = br#"{"canon":{"post":{"artif"#;
    fs::OpenOptions::new()
        .append(true)
        .open(posts_file(&root, &b))
        .unwrap()
        .write_all(torn)
        .unwrap();
    let mut store = open(&root);
    // Reads serve the complete lines.
    assert_eq!(seqs(&list(&mut store, json!({ "boardId": b }))), [2, 1]);
    // The torn post was never acknowledged: its re-send creates it.
    let resent: PostResult = ok(post(&mut store, manual(&b, "cli", "r2", input("t2", "e2"))));
    assert!(!resent.duplicate);
    assert_eq!(resent.seq, Seq(3));
    let bytes = fs::read(posts_file(&root, &b)).unwrap();
    assert_eq!(bytes.iter().filter(|&&c| c == b'\n').count(), 3);
    assert!(bytes.ends_with(b"\n"));
    let quarantine: Vec<_> = fs::read_dir(root.join(&b).join("quarantine"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(quarantine.len(), 1);
    assert_eq!(fs::read(&quarantine[0]).unwrap(), torn);
    drop(store);
    let mut store = open(&root);
    assert_eq!(seqs(&list(&mut store, json!({ "boardId": b }))), [3, 2, 1]);
}

#[test]
fn ac1_corrupt_middle_line_stops_writes() {
    let root = temp_root("corrupt");
    let b = {
        let mut store = open(&root);
        let b = board(&mut store, "b").board_id;
        post_n(&mut store, &b, 3);
        b
    };
    let path = posts_file(&root, &b);
    let text = fs::read_to_string(&path).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines[1] = "{not json";
    let corrupted = format!("{}\n", lines.join("\n"));
    fs::write(&path, &corrupted).unwrap();
    let mut store = open(&root);
    // Reads serve the valid prefix.
    let page = list(&mut store, json!({ "boardId": b }));
    assert_eq!(seqs(&page), [1]);
    let err = post(&mut store, manual(&b, "cli", "new", input("n", "en"))).unwrap_err();
    assert_eq!(err.kind, TimelineErrorKind::CorruptStore);
    assert_eq!(err.line, Some(2));
    assert_eq!(err.data()["line"], json!(2));
    assert_eq!(fs::read_to_string(&path).unwrap(), corrupted);
}

#[test]
fn ac1_storage_full_rejects_without_writing() {
    let root = temp_root("full");
    let mut store = Store::open(
        root.clone(),
        INSTANCE.to_owned(),
        StoreLimits {
            // One post's line (its body twice: in the post and the canon) is
            // about 5 KB: the first fits, the second does not.
            max_board_bytes: 7000,
        },
        Box::new(RealIo),
    )
    .unwrap();
    let b = board(&mut store, "b").board_id;
    let big = |request: &str| {
        let mut post = input(request, request);
        post["bodyMarkdown"] = json!("x".repeat(2000));
        post
    };
    ok::<PostResult>(post(&mut store, manual(&b, "cli", "r1", big("r1"))));
    let before = fs::read(posts_file(&root, &b)).unwrap();
    let err = post(&mut store, manual(&b, "cli", "r2", big("r2"))).unwrap_err();
    assert_eq!(err.kind, TimelineErrorKind::StorageFull);
    assert_eq!(fs::read(posts_file(&root, &b)).unwrap(), before);
    assert_eq!(list(&mut store, json!({ "boardId": b })).head_seq, Seq(1));
}

/// Fails the next `sync_data` once armed.
struct FailSync(Arc<AtomicBool>);

impl StoreIo for FailSync {
    fn sync_data(&self, file: &fs::File) -> io::Result<()> {
        if self.0.swap(false, Ordering::SeqCst) {
            Err(io::Error::other("injected sync failure"))
        } else {
            file.sync_data()
        }
    }
}

#[test]
fn ac1_sync_failure_rebuilds_before_next_seq() {
    let root = temp_root("sync-fail");
    let armed = Arc::new(AtomicBool::new(false));
    let mut store = Store::open(
        root.clone(),
        INSTANCE.to_owned(),
        StoreLimits::default(),
        Box::new(FailSync(armed.clone())),
    )
    .unwrap();
    let b = board(&mut store, "b").board_id;
    armed.store(true, Ordering::SeqCst);
    let err = post(&mut store, manual(&b, "cli", "r1", input("t1", "e1"))).unwrap_err();
    assert_eq!(err.kind, TimelineErrorKind::IoError);
    // The line reached the file but was never acknowledged.
    assert!(fs::metadata(posts_file(&root, &b)).unwrap().len() > 0);
    // The re-send is created afresh at seq 1, not 2, and not a duplicate.
    let resent: PostResult = ok(post(&mut store, manual(&b, "cli", "r1", input("t1", "e1"))));
    assert!(!resent.duplicate);
    assert_eq!(resent.seq, Seq(1));
    let next: PostResult = ok(post(&mut store, manual(&b, "cli", "r2", input("t2", "e2"))));
    assert_eq!(next.seq, Seq(2));
    let text = fs::read_to_string(posts_file(&root, &b)).unwrap();
    assert_eq!(text.lines().count(), 2);
}

#[test]
fn ac1_limits_rejected_not_truncated() {
    let root = temp_root("limits");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    let with = |field: &str, value: Value| {
        let mut post = input("t", "e");
        post[field] = value;
        manual(&b, "cli", "r", post)
    };
    for params in [
        with("title", json!("t".repeat(121))),
        with("title", json!(" \t\n")),
        with("summary", json!("s".repeat(401))),
        with("summary", json!("")),
        with("bodyMarkdown", json!("b".repeat(MAX_BODY_BYTES + 1))),
    ] {
        assert_eq!(
            kind(post(&mut store, params)),
            TimelineErrorKind::InvalidParams
        );
    }
    // The store checks again for callers that skip `TimelineCall::parse`.
    let Some(TimelineCall::Post(mut params)) =
        TimelineCall::parse("noa.timeline.post", with("title", json!("ok"))).unwrap()
    else {
        panic!("post");
    };
    params.post.title = "t".repeat(121);
    assert_eq!(
        kind(store.handle(TimelineCall::Post(params), &[])),
        TimelineErrorKind::InvalidParams
    );
    assert_eq!(list(&mut store, json!({ "boardId": b })).head_seq, Seq(0));
    // At the limits, accepted whole.
    let mut full = input(&"t".repeat(120), "e-full");
    full["summary"] = json!("日".repeat(400));
    full["bodyMarkdown"] = json!("b".repeat(MAX_BODY_BYTES));
    let stored: PostResult = ok(post(&mut store, manual(&b, "cli", "full", full)));
    let got: GetResult = ok(call(
        &mut store,
        "noa.timeline.get",
        json!({ "boardId": b, "postId": stored.post_id }),
        &[],
    ));
    assert_eq!(got.post.input.body_markdown.len(), MAX_BODY_BYTES);
    assert_eq!(got.post.input.summary.chars().count(), 400);
}

#[test]
fn ac1_list_and_changes_pages_have_no_gaps_when_posts_arrive_between_pages() {
    let root = temp_root("pages");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    post_n(&mut store, &b, 10);
    let mut extra = 0;
    let mut more = |store: &mut Store| {
        extra += 1;
        ok::<PostResult>(post(
            store,
            manual(
                &b,
                "cli",
                &format!("x{extra}"),
                input("x", &format!("ex{extra}")),
            ),
        ));
    };

    // Descending: arrivals between pages land above the first page.
    let first = list(&mut store, json!({ "boardId": b, "limit": 3 }));
    assert_eq!(seqs(&first), [10, 9, 8]);
    assert!(first.has_more);
    let mut seen = seqs(&first);
    let mut before = first.next_before_seq.unwrap();
    loop {
        more(&mut store);
        let page = list(
            &mut store,
            json!({ "boardId": b, "limit": 3, "beforeSeq": before.0.to_string() }),
        );
        assert!(page.posts.iter().all(|p| p.seq.0 <= page.head_seq.0));
        seen.extend(seqs(&page));
        if !page.has_more {
            break;
        }
        before = page.next_before_seq.unwrap();
    }
    assert_eq!(seen, (1..=10).rev().collect::<Vec<_>>());

    // Ascending: every post once, the arrivals included, in order.
    let mut seen = Vec::new();
    let mut after = 0;
    for round in 0.. {
        let page = changes(&mut store, &b, after, 4);
        seen.extend(seqs(&page));
        after = page.next_after_seq.unwrap().0;
        if round < 2 {
            more(&mut store);
        }
        if !page.has_more && round >= 2 {
            break;
        }
    }
    let head = list(&mut store, json!({ "boardId": b })).head_seq.0;
    assert_eq!(seen, (1..=head).collect::<Vec<_>>());
}

#[test]
fn ac1_list_byte_cap_sets_has_more() {
    let root = temp_root("byte-cap");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    for i in 0..10 {
        let mut post = input("t", &format!("e{i}"));
        post["taskRef"] = json!({ "kind": "local", "id": "t", "title": "w".repeat(60_000) });
        ok::<PostResult>(super::tests::post(
            &mut store,
            manual(&b, "cli", &format!("r{i}"), post),
        ));
    }
    let page = list(&mut store, json!({ "boardId": b, "limit": 50 }));
    assert!(page.has_more);
    assert!(page.posts.len() < 10 && !page.posts.is_empty());
    assert!(serde_json::to_vec(&page).unwrap().len() <= RESULT_BUDGET);
    let next = list(
        &mut store,
        json!({ "boardId": b, "limit": 50,
                "beforeSeq": page.next_before_seq.unwrap().0.to_string() }),
    );
    assert_eq!(
        next.posts[0].seq.0,
        page.posts.last().unwrap().seq.0 - 1,
        "the next page starts right after the cap"
    );
    let ch = changes(&mut store, &b, 0, 200);
    assert!(ch.has_more);
    assert!(serde_json::to_vec(&ch).unwrap().len() <= RESULT_BUDGET);
}

#[test]
fn ac1_boards_pages_reach_every_board_once() {
    let root = temp_root("boards");
    let mut store = open(&root);
    let all: Vec<String> = (0..205)
        .map(|i| board(&mut store, &format!("b{i}")).board_id)
        .collect();
    let boards = |store: &mut Store, before: Option<Value>| -> BoardsResult {
        let mut params = json!({ "limit": 200 });
        if let Some(before) = before {
            params["before"] = before;
        }
        ok(call(store, "noa.timeline.boards", params, &[]))
    };
    let first = boards(&mut store, None);
    assert_eq!(first.boards.len(), 200);
    assert!(first.has_more);
    // A post on an already listed board between pages.
    std::thread::sleep(Duration::from_millis(5));
    let listed = first.boards[10].board_id.clone();
    ok::<PostResult>(post(
        &mut store,
        manual(&listed, "cli", "r", input("t", "e")),
    ));
    let cursor = serde_json::to_value(first.next_before.clone().unwrap()).unwrap();
    let second = boards(&mut store, Some(cursor));
    assert!(!second.has_more);
    let mut seen: Vec<String> = first
        .boards
        .iter()
        .chain(&second.boards)
        .map(|b| b.board_id.clone())
        .collect();
    let count = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), count, "no board twice");
    let mut expected = all.clone();
    expected.sort();
    assert_eq!(seen, expected, "every board once");
    // A fresh pass lists the posted board first.
    assert_eq!(boards(&mut store, None).boards[0].board_id, listed);
}

#[test]
fn ac1_last_activity_never_decreases() {
    let root = temp_root("activity");
    let (b, first_at) = {
        let mut store = open(&root);
        let b = board(&mut store, "b").board_id;
        let posts = post_n(&mut store, &b, 2);
        (b, posts[0].created_at.clone())
    };
    let path = posts_file(&root, &b);
    let text = fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut second: Value = serde_json::from_str(&lines[1]).unwrap();
    second["post"]["createdAt"] = json!("2000-01-01T00:00:00.000Z");
    lines[1] = serde_json::to_string(&second).unwrap();
    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    let mut store = open(&root);
    let listing: BoardsResult = ok(call(&mut store, "noa.timeline.boards", json!({}), &[]));
    assert_eq!(
        listing.boards[0].last_activity_at.as_deref(),
        Some(first_at.as_str())
    );
}

#[test]
fn ac1_modes_0700_0600() {
    let root = temp_root("modes");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    post_n(&mut store, &b, 1);
    drop(store);
    fs::OpenOptions::new()
        .append(true)
        .open(posts_file(&root, &b))
        .unwrap()
        .write_all(b"{")
        .unwrap();
    let mut store = open(&root);
    post_n(&mut store, &b, 2);
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let dir = root.join(&b);
    assert_eq!(mode(&root), 0o700);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("quarantine")), 0o700);
    for file in ["meta.json", "posts.jsonl", "lock"] {
        assert_eq!(mode(&dir.join(file)), 0o600, "{file}");
    }
    for entry in fs::read_dir(dir.join("quarantine")).unwrap() {
        assert_eq!(mode(&entry.unwrap().path()), 0o600);
    }
}

#[test]
fn ac1_bad_board_id_rejected_before_path() {
    let root = temp_root("bad-id");
    let mut store = open(&root);
    assert_eq!(
        kind(call(
            &mut store,
            "noa.timeline.list",
            json!({ "boardId": "bd-../../../../etc" }),
            &[]
        )),
        TimelineErrorKind::InvalidParams
    );
    // Past the parser, the store checks again before any path exists.
    let call_list = |board: &str| {
        TimelineCall::List(noa_ipc::timeline::ListParams {
            before_seq: None,
            board_id: board.to_owned(),
            filter: None,
            limit: None,
            query: None,
        })
    };
    assert_eq!(
        kind(store.handle(call_list("../escape"), &[])),
        TimelineErrorKind::InvalidParams
    );
    assert_eq!(
        kind(store.handle(call_list("bd-0123456789abcdef"), &[])),
        TimelineErrorKind::NotFound
    );
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    assert!(!root.parent().unwrap().join("escape").exists());
}

#[test]
fn ac1_runs_on_service_thread() {
    let root = temp_root("thread");
    let probe = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let installed = probe.clone();
    let handle = service::start_with(
        root,
        INSTANCE.to_owned(),
        StoreLimits::default(),
        Box::new(RealIo),
        move |store| store.thread_probe = Some(installed),
    )
    .unwrap();
    let call = TimelineCall::parse("noa.timeline.boards", Value::Null)
        .unwrap()
        .unwrap();
    ok::<BoardsResult>(handle.call(call, Arc::new(Vec::new())));
    assert_eq!(*probe.lock(), [service::THREAD_NAME.to_owned()]);
    assert_eq!(service::THREAD_NAME, "noa-timeline");
}

#[test]
fn ac1_service_busy_and_outcome_unknown_on_timeout() {
    let root = temp_root("timeout");
    let (release, gate) = crossbeam_channel::bounded::<()>(0);
    let handle = service::start_with(
        root,
        INSTANCE.to_owned(),
        StoreLimits::default(),
        Box::new(RealIo),
        move |_| {
            // Holds the service before its first job.
            let _ = gate.recv();
        },
    )
    .unwrap();
    let write = TimelineCall::parse(
        "noa.timeline.registerRun",
        json!({ "requestId": "q1", "publishPolicy": { "owner": "runner", "producerId": "r" } }),
    )
    .unwrap()
    .unwrap();
    let err = handle
        .call_with_wait(write, Arc::new(Vec::new()), Duration::from_millis(20))
        .unwrap_err();
    assert_eq!(err.kind, TimelineErrorKind::OutcomeUnknown);
    assert_eq!(err.request_id.as_deref(), Some("q1"));
    let read = TimelineCall::parse("noa.timeline.boards", Value::Null)
        .unwrap()
        .unwrap();
    let err = handle
        .call_with_wait(read, Arc::new(Vec::new()), Duration::from_millis(20))
        .unwrap_err();
    assert_eq!(err.kind, TimelineErrorKind::Busy);
    release.send(()).unwrap();
    // The timed-out write still lands; its re-send returns the same run.
    let again = TimelineCall::parse(
        "noa.timeline.registerRun",
        json!({ "requestId": "q1", "publishPolicy": { "owner": "runner", "producerId": "r" } }),
    )
    .unwrap()
    .unwrap();
    let result: RegisterRunResult = ok(handle.call(again, Arc::new(Vec::new())));
    assert!(!result.created);
}

#[test]
fn ac1_second_instance_lock_is_busy() {
    let root = temp_root("lock");
    let mut first = open(&root);
    let b = board(&mut first, "b").board_id;
    post_n(&mut first, &b, 1);
    let mut second = open_as(&root, "instance-b");
    assert_eq!(
        kind(post(
            &mut second,
            manual(&b, "cli", "other", input("o", "eo"))
        )),
        TimelineErrorKind::Busy
    );
    // Reads need no lock and see what the first instance writes.
    assert_eq!(seqs(&list(&mut second, json!({ "boardId": b }))), [1]);
    post_n(&mut first, &b, 2);
    assert_eq!(seqs(&list(&mut second, json!({ "boardId": b }))), [2, 1]);
    // A board the first instance creates later is listed too.
    let later = board(&mut first, "later").board_id;
    let listing: BoardsResult = ok(call(&mut second, "noa.timeline.boards", json!({}), &[]));
    assert!(listing.boards.iter().any(|info| info.board_id == later));
    drop(first);
    let next: PostResult = ok(post(
        &mut second,
        manual(&b, "cli", "other", input("o", "eo")),
    ));
    assert_eq!(next.seq, Seq(3));
}

#[test]
fn ac1_context_runs_bounded_to_response() {
    let root = temp_root("context-bound");
    let mut store = open(&root);
    let live = panels(&[1, 2]);
    let mut last = None;
    for i in 0..60 {
        let r: RegisterRunResult = ok(register(
            &mut store,
            &format!("run-{i}"),
            Some(1),
            Some(2),
            json!({ "taskRef": { "kind": "local", "id": format!("t{i}"),
                                 "title": "w".repeat(5_000) } }),
            &live,
        ));
        last = Some(r);
    }
    let last = last.unwrap();
    let by_board = context(&mut store, json!({ "boardId": last.board_id }), &live);
    assert!(serde_json::to_vec(&by_board).unwrap().len() <= RESULT_BUDGET);
    let runs = &by_board.workers[0].runs;
    assert!(!runs.is_empty() && runs.len() < 60);
    // The most recent runs, in registration order.
    assert_eq!(runs.last().unwrap().run_id, last.run_id);
    let ids: Vec<_> = runs
        .iter()
        .map(|r| r.task_ref.as_ref().unwrap().id.clone())
        .collect();
    let expected: Vec<_> = (60 - runs.len()..60).map(|i| format!("t{i}")).collect();
    assert_eq!(ids, expected);
    let by_pane = context(&mut store, json!({ "paneId": "2" }), &live);
    assert!(serde_json::to_vec(&by_pane).unwrap().len() <= RESULT_BUDGET);
    let me = by_pane.me.unwrap();
    assert!(me.runs.len() >= 2);
    assert_eq!(me.runs.last().unwrap().run_id, last.run_id);
}

#[test]
fn ac1_list_filter_and_query() {
    let root = temp_root("query");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    for i in 0..30 {
        let mut post = input(&format!("t{i}"), &format!("e{i}"));
        post["bodyMarkdown"] = json!(if i % 10 == 3 {
            format!("needle Hidden {i}")
        } else {
            "plain".to_owned()
        });
        if i == 5 {
            post["outcome"] = json!("failed");
            post["title"] = json!("日本語のタイトル");
        }
        ok::<PostResult>(super::tests::post(
            &mut store,
            manual(&b, "cli", &format!("r{i}"), post),
        ));
    }
    let page = list(&mut store, json!({ "boardId": b, "query": "HIDDEN" }));
    assert_eq!(seqs(&page), [24, 14, 4]);
    let page = list(&mut store, json!({ "boardId": b, "query": "本語" }));
    assert_eq!(seqs(&page), [6]);
    let page = list(
        &mut store,
        json!({ "boardId": b, "filter": { "outcomes": ["failed"] } }),
    );
    assert_eq!(seqs(&page), [6]);
    let page = list(
        &mut store,
        json!({ "boardId": b, "query": "needle", "limit": 2 }),
    );
    assert_eq!(seqs(&page), [24, 14]);
    assert!(page.has_more);
    let page = list(
        &mut store,
        json!({ "boardId": b, "query": "needle", "beforeSeq": "14" }),
    );
    assert_eq!(seqs(&page), [4]);
    assert!(!page.has_more);
}

// ---------------------------------------------------------------------------
// AC-4 (store side)

#[test]
fn ac4_register_run_idempotent() {
    let root = temp_root("reg-idem");
    let mut store = open(&root);
    let live = panels(&[1, 2]);
    let first: RegisterRunResult = ok(register(
        &mut store,
        "q",
        Some(1),
        Some(2),
        json!({}),
        &live,
    ));
    assert!(first.created);
    assert_eq!(first.producer_id, first.worker_id.clone().unwrap());
    let again: RegisterRunResult = ok(register(
        &mut store,
        "q",
        Some(1),
        Some(2),
        json!({}),
        &live,
    ));
    assert!(!again.created);
    assert_eq!(
        (&again.board_id, &again.run_id, &again.worker_id),
        (&first.board_id, &first.run_id, &first.worker_id)
    );
    drop(store);
    let mut store = open(&root);
    let replayed: RegisterRunResult = ok(register(
        &mut store,
        "q",
        Some(1),
        Some(2),
        json!({}),
        &live,
    ));
    assert_eq!(replayed.run_id, first.run_id);
    let ctx = context(&mut store, json!({ "paneId": "2" }), &live);
    assert_eq!(ctx.me.unwrap().runs.len(), 1);
}

#[test]
fn ac4_register_run_other_payload_conflicts() {
    let root = temp_root("reg-conflict");
    let mut store = open(&root);
    let live = panels(&[1, 2]);
    let first: RegisterRunResult = ok(register(
        &mut store,
        "q",
        Some(1),
        Some(2),
        json!({}),
        &live,
    ));
    let err = register(
        &mut store,
        "q",
        Some(1),
        Some(2),
        json!({ "taskRef": { "kind": "local", "id": "other" } }),
        &live,
    )
    .unwrap_err();
    assert_eq!(err.kind, TimelineErrorKind::Conflict);
    assert_eq!(err.existing_run_id, Some(first.run_id));
}

#[test]
fn ac4_worker_owner_needs_a_worker_or_producer() {
    let root = temp_root("reg-producer");
    let mut store = open(&root);
    let live = panels(&[1]);
    assert_eq!(
        kind(register(&mut store, "q", Some(1), None, json!({}), &live)),
        TimelineErrorKind::InvalidParams
    );
    assert!(
        fs::read_dir(&root).unwrap().next().is_none(),
        "no board made"
    );
    let named: RegisterRunResult = ok(register(
        &mut store,
        "q2",
        Some(1),
        None,
        json!({ "publishPolicy": { "owner": "worker", "producerId": "wk-0123456789abcdef" } }),
        &live,
    ));
    assert_eq!(named.producer_id, "wk-0123456789abcdef");
    // A pane that is not open is not bound.
    assert_eq!(
        kind(register(
            &mut store,
            "q3",
            Some(1),
            Some(42),
            json!({}),
            &live
        )),
        TimelineErrorKind::NotFound
    );
}

#[test]
fn ac4_child_inherits_parent_board() {
    let root = temp_root("inherit");
    let mut store = open(&root);
    let live = panels(&[1, 2, 3, 9, 10]);
    let parent: RegisterRunResult = ok(register(
        &mut store,
        "a",
        Some(1),
        Some(2),
        json!({}),
        &live,
    ));
    let child: RegisterRunResult = ok(register(
        &mut store,
        "b",
        Some(2),
        Some(3),
        json!({}),
        &live,
    ));
    assert_eq!(child.board_id, parent.board_id);
    let ctx = context(&mut store, json!({ "paneId": "3" }), &live);
    assert_eq!(ctx.board_id, parent.board_id);
    assert_eq!(ctx.me.as_ref().unwrap().role, "worker");
    assert_eq!(ctx.workers.len(), 2);
    let root_ctx = context(&mut store, json!({ "paneId": "1" }), &live);
    assert_eq!(root_ctx.me.unwrap().role, "root");
    // An unbound orchestrator starts its own board.
    let other: RegisterRunResult = ok(register(
        &mut store,
        "c",
        Some(9),
        Some(10),
        json!({}),
        &live,
    ));
    assert_ne!(other.board_id, parent.board_id);
    // A new run on the same pane is the same worker.
    let next: RegisterRunResult = ok(register(
        &mut store,
        "d",
        Some(1),
        Some(2),
        json!({}),
        &live,
    ));
    assert_eq!(next.worker_id, parent.worker_id);
    assert_ne!(next.run_id, parent.run_id);
}

#[test]
fn ac4_root_rebind_after_restart_with_board_id() {
    let root = temp_root("rebind");
    let runner = json!({ "publishPolicy": { "owner": "runner", "producerId": "review:s1" } });
    let first: RegisterRunResult = {
        let mut store = open_as(&root, "before-restart");
        ok(register(
            &mut store,
            "review-s1-run",
            Some(1),
            None,
            runner.clone(),
            &panels(&[1]),
        ))
    };
    let mut store = open_as(&root, "after-restart");
    let live = panels(&[7]);
    let mut resend = runner.clone();
    resend["boardId"] = json!(first.board_id);
    let again: RegisterRunResult = ok(register(
        &mut store,
        "review-s1-run",
        Some(7),
        None,
        resend,
        &live,
    ));
    assert!(!again.created);
    assert_eq!(
        (&again.board_id, &again.run_id),
        (&first.board_id, &first.run_id)
    );
    let ctx = context(&mut store, json!({ "paneId": "7" }), &live);
    assert_eq!(ctx.board_id, first.board_id);
    assert_eq!(ctx.me.unwrap().role, "root");
    assert!(!ctx.archived);
    // The rebound root resolves its board for new runs.
    let next: RegisterRunResult = ok(register(
        &mut store,
        "review-s1-run2",
        Some(7),
        None,
        runner,
        &live,
    ));
    assert_eq!(next.board_id, first.board_id);
}

#[test]
fn ac4_retry_links_retry_of() {
    let root = temp_root("retry");
    let mut store = open(&root);
    let live = panels(&[1, 2, 3]);
    let old: RegisterRunResult = ok(register(
        &mut store,
        "a",
        Some(1),
        Some(2),
        json!({}),
        &live,
    ));
    let retry: RegisterRunResult = ok(register(
        &mut store,
        "b",
        Some(1),
        Some(3),
        json!({ "retryOfRunId": old.run_id }),
        &live,
    ));
    let ctx = context(&mut store, json!({ "paneId": "3" }), &live);
    let run: &RunInfo = &ctx.me.as_ref().unwrap().runs[0];
    assert_eq!(run.run_id, retry.run_id);
    assert_eq!(run.retry_of_run_id.as_deref(), Some(old.run_id.as_str()));
    assert_eq!(
        kind(register(
            &mut store,
            "c",
            Some(1),
            Some(3),
            json!({ "retryOfRunId": "rn-ffffffffffffffff" }),
            &live,
        )),
        TimelineErrorKind::InvalidParams
    );
}

#[test]
fn ac4_live_only_when_instance_pane_surface_match() {
    let root = temp_root("live");
    let mut store = open(&root);
    let live = panels(&[1, 2]);
    let run: RegisterRunResult = ok(register(
        &mut store,
        "a",
        Some(1),
        Some(2),
        json!({}),
        &live,
    ));
    let by_board = json!({ "boardId": run.board_id });
    let ctx = context(&mut store, by_board.clone(), &live);
    assert!(ctx.workers[0].live);
    assert_eq!(ctx.workers[0].pane_id.map(|p| p.0), Some(2));
    assert!(!ctx.archived);
    // Same pane number, another surface (a reused number).
    let reused = vec![panel(1, "s1"), panel(2, "other")];
    let ctx = context(&mut store, by_board.clone(), &reused);
    assert!(!ctx.workers[0].live);
    assert_eq!(ctx.workers[0].pane_id, None);
    // The pane is gone.
    let ctx = context(&mut store, by_board.clone(), &panels(&[1]));
    assert!(!ctx.workers[0].live);
    // Another instance (a restart): nothing is live, the board is an archive.
    drop(store);
    let mut store = open_as(&root, "instance-b");
    let ctx = context(&mut store, by_board, &live);
    assert!(!ctx.workers[0].live);
    assert!(ctx.archived);
    assert_eq!(
        kind(call(
            &mut store,
            "noa.timeline.context",
            json!({ "paneId": "2" }),
            &live
        )),
        TimelineErrorKind::NotFound
    );
}

#[test]
fn ac4_posts_only_from_the_runs_producer_and_policy_change_is_recorded() {
    let root = temp_root("producer");
    let mut store = open(&root);
    let live = panels(&[1, 2]);
    let run: RegisterRunResult = ok(register(
        &mut store,
        "a",
        Some(1),
        Some(2),
        json!({}),
        &live,
    ));
    let summary = |producer: &str, request: &str| {
        let mut post = input("done", request);
        post["kind"] = json!("summary");
        post["sourceRef"]["namespace"] = json!("worker");
        json!({
            "boardId": run.board_id, "runId": run.run_id, "producerId": producer,
            "requestId": request, "post": post,
        })
    };
    assert_eq!(
        kind(post(&mut store, summary("someone-else", "r1"))),
        TimelineErrorKind::InvalidParams
    );
    // The orchestrator takes over a result-only worker's run.
    let policy = json!({
        "boardId": run.board_id, "runId": run.run_id, "requestId": "take-1",
        "publishPolicy": { "owner": "manual", "producerId": "cli:orchestrator" },
    });
    let info: RunInfo = ok(call(
        &mut store,
        "noa.timeline.setPublishPolicy",
        policy.clone(),
        &[],
    ));
    assert_eq!(
        info.publish_policy.producer_id.as_deref(),
        Some("cli:orchestrator")
    );
    let again: RunInfo = ok(call(
        &mut store,
        "noa.timeline.setPublishPolicy",
        policy.clone(),
        &[],
    ));
    assert_eq!(again, info);
    let mut other = policy;
    other["publishPolicy"]["producerId"] = json!("cli:someone");
    assert_eq!(
        kind(call(
            &mut store,
            "noa.timeline.setPublishPolicy",
            other,
            &[]
        )),
        TimelineErrorKind::Conflict
    );
    let worker = run.worker_id.clone().unwrap();
    assert_eq!(
        kind(post(&mut store, summary(&worker, "r2"))),
        TimelineErrorKind::InvalidParams
    );
    let stored: PostResult = ok(post(&mut store, summary("cli:orchestrator", "r3")));
    let got: GetResult = ok(call(
        &mut store,
        "noa.timeline.get",
        json!({ "boardId": run.board_id, "postId": stored.post_id }),
        &[],
    ));
    assert_eq!(got.post.worker_id.as_deref(), Some(worker.as_str()));
    assert_eq!(got.post.author.agent_kind.as_deref(), Some("claude"));
    assert_eq!(
        got.post.provenance,
        noa_ipc::timeline::Provenance::Registered
    );
    let ctx = context(&mut store, json!({ "paneId": "2" }), &live);
    assert_eq!(
        ctx.me.unwrap().runs[0].summary_post_id.as_deref(),
        Some(stored.post_id.as_str())
    );
}

// ---------------------------------------------------------------------------
// Faults: failed syncs and reads, and another instance's write just before
// a board lock is taken.

/// What a [`FaultIo`] injects and records.
#[derive(Default)]
struct Faults {
    /// Every `sync_data` fails while set.
    fail_syncs: AtomicBool,
    /// Every directory sync fails while set.
    fail_dir_syncs: AtomicBool,
    /// `sync_data` calls, failed or not.
    syncs: AtomicU64,
    /// The next replay read fails.
    fail_read: AtomicBool,
    /// Run once, just before the next board lock is taken.
    before_lock: parking_lot::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// (directory, thread name) of every directory sync.
    dir_syncs: parking_lot::Mutex<Vec<(PathBuf, String)>>,
}

struct FaultIo(Arc<Faults>);

impl StoreIo for FaultIo {
    fn sync_data(&self, file: &fs::File) -> io::Result<()> {
        self.0.syncs.fetch_add(1, Ordering::SeqCst);
        if self.0.fail_syncs.load(Ordering::SeqCst) {
            return Err(io::Error::other("injected sync failure"));
        }
        file.sync_data()
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        let thread = std::thread::current().name().unwrap_or_default().to_owned();
        self.0.dir_syncs.lock().push((dir.to_path_buf(), thread));
        if self.0.fail_dir_syncs.load(Ordering::SeqCst) {
            return Err(io::Error::other("injected directory sync failure"));
        }
        RealIo.sync_dir(dir)
    }

    fn lock(&self, file: &fs::File) -> io::Result<()> {
        let hook = self.0.before_lock.lock().take();
        if let Some(hook) = hook {
            hook();
        }
        RealIo.lock(file)
    }

    fn open_read(&self, path: &Path) -> io::Result<fs::File> {
        if self.0.fail_read.swap(false, Ordering::SeqCst) {
            return Err(io::Error::other("injected read failure"));
        }
        fs::File::open(path)
    }
}

fn open_faulty(root: &Path, instance: &str, faults: &Arc<Faults>) -> Store {
    Store::open(
        root.to_path_buf(),
        instance.to_owned(),
        StoreLimits::default(),
        Box::new(FaultIo(Arc::clone(faults))),
    )
    .unwrap()
}

fn runner_run(store: &mut Store, request: &str, board: Option<&str>) -> RegisterRunResult {
    let mut params = json!({
        "requestId": request,
        "publishPolicy": { "owner": "runner", "producerId": "runner:t" },
    });
    if let Some(board) = board {
        params["boardId"] = json!(board);
    }
    ok(call(store, "noa.timeline.registerRun", params, &[]))
}

#[test]
fn ac1_resend_after_failed_sync_is_synced_before_ack() {
    let root = temp_root("resend-sync");
    let first = Arc::new(Faults::default());
    let b = {
        let mut store = open_faulty(&root, INSTANCE, &first);
        let b = board(&mut store, "b").board_id;
        first.fail_syncs.store(true, Ordering::SeqCst);
        assert_eq!(
            kind(post(&mut store, manual(&b, "cli", "r1", input("t1", "e1")))),
            TimelineErrorKind::IoError
        );
        b
    };
    // The process ended with the line in the file, its sync failed. After
    // a restart the disk still refuses syncs: the re-send is no success.
    let faults = Arc::new(Faults::default());
    faults.fail_syncs.store(true, Ordering::SeqCst);
    let mut store = open_faulty(&root, INSTANCE, &faults);
    let before = faults.syncs.load(Ordering::SeqCst);
    assert_eq!(
        kind(post(&mut store, manual(&b, "cli", "r1", input("t1", "e1")))),
        TimelineErrorKind::IoError
    );
    assert!(faults.syncs.load(Ordering::SeqCst) > before);
    // Once a sync succeeds, that line is the post.
    faults.fail_syncs.store(false, Ordering::SeqCst);
    let again: PostResult = ok(post(&mut store, manual(&b, "cli", "r1", input("t1", "e1"))));
    assert!(again.duplicate);
    assert_eq!(again.seq, Seq(1));
    // A line this store saw synced is not synced again for a re-send.
    let synced = faults.syncs.load(Ordering::SeqCst);
    let again: PostResult = ok(post(&mut store, manual(&b, "cli", "r1", input("t1", "e1"))));
    assert!(again.duplicate);
    assert_eq!(faults.syncs.load(Ordering::SeqCst), synced);
}

#[test]
fn ac1_failed_read_during_rebuild_keeps_the_board_poisoned() {
    let root = temp_root("rebuild-read");
    let faults = Arc::new(Faults::default());
    let mut store = open_faulty(&root, INSTANCE, &faults);
    let b = board(&mut store, "b").board_id;
    let first: PostResult = ok(post(&mut store, manual(&b, "cli", "r1", input("t1", "e1"))));
    assert_eq!(first.seq, Seq(1));
    faults.fail_syncs.store(true, Ordering::SeqCst);
    assert_eq!(
        kind(post(&mut store, manual(&b, "cli", "r2", input("t2", "e2")))),
        TimelineErrorKind::IoError
    );
    faults.fail_syncs.store(false, Ordering::SeqCst);
    // The rebuild cannot read the file: nothing is written, and seq 1
    // stays taken.
    faults.fail_read.store(true, Ordering::SeqCst);
    assert_eq!(
        kind(post(&mut store, manual(&b, "cli", "r3", input("t3", "e3")))),
        TimelineErrorKind::IoError
    );
    let next: PostResult = ok(post(&mut store, manual(&b, "cli", "r3", input("t3", "e3"))));
    assert_eq!(next.seq, Seq(2));
    drop(store);
    let mut store = open(&root);
    assert_eq!(seqs(&list(&mut store, json!({ "boardId": b }))), [2, 1]);
    let after: PostResult = ok(post(&mut store, manual(&b, "cli", "r4", input("t4", "e4"))));
    assert_eq!(after.seq, Seq(3));
}

#[test]
fn ac1_failed_catch_up_after_lock_drops_the_writer() {
    let root = temp_root("lock-catch-up");
    let mut first = open(&root);
    let b = board(&mut first, "b").board_id;
    post_n(&mut first, &b, 1);
    let faults = Arc::new(Faults::default());
    let mut second = open_faulty(&root, "instance-b", &faults);
    assert_eq!(seqs(&list(&mut second, json!({ "boardId": b }))), [1]);
    // After the second store's read and before its lock, the first
    // commits seq 2 and exits; the second's read under the lock fails.
    let hook_faults = Arc::clone(&faults);
    let hook_board = b.clone();
    *faults.before_lock.lock() = Some(Box::new(move || {
        let mut first = first;
        ok::<PostResult>(post(
            &mut first,
            manual(&hook_board, "cli", "a2", input("a2", "ea2")),
        ));
        drop(first);
        hook_faults.fail_read.store(true, Ordering::SeqCst);
    }));
    assert_eq!(
        kind(post(
            &mut second,
            manual(&b, "cli", "b1", input("b1", "eb1"))
        )),
        TimelineErrorKind::IoError
    );
    let next: PostResult = ok(post(
        &mut second,
        manual(&b, "cli", "b1", input("b1", "eb1")),
    ));
    assert_eq!(next.seq, Seq(3));
    drop(second);
    let mut store = open(&root);
    assert_eq!(seqs(&list(&mut store, json!({ "boardId": b }))), [3, 2, 1]);
}

#[test]
fn ac1_mistyped_middle_line_stops_replay() {
    for (field, value) in [
        ("canon", Value::Null),
        ("post", json!({ "nextActions": 5 })),
    ] {
        let root = temp_root("mistyped");
        let b = {
            let mut store = open(&root);
            let b = board(&mut store, "b").board_id;
            post_n(&mut store, &b, 3);
            b
        };
        let path = posts_file(&root, &b);
        let text = fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let mut line: Value = serde_json::from_str(&lines[1]).unwrap();
        if field == "canon" {
            line["canon"] = value;
        } else {
            line["post"]["nextActions"] = value["nextActions"].clone();
        }
        lines[1] = line.to_string();
        let mistyped = format!("{}\n", lines.join("\n"));
        fs::write(&path, &mistyped).unwrap();
        let mut store = open(&root);
        assert_eq!(
            seqs(&list(&mut store, json!({ "boardId": b }))),
            [1],
            "{field}"
        );
        let err = post(&mut store, manual(&b, "cli", "new", input("n", "en"))).unwrap_err();
        assert_eq!(err.kind, TimelineErrorKind::CorruptStore, "{field}");
        assert_eq!(err.line, Some(2), "{field}");
        assert_eq!(fs::read_to_string(&path).unwrap(), mistyped);
    }
}

#[test]
fn ac1_store_dirs_are_made_durable_on_the_service_thread() {
    // Neither `<tmp>/noa-timeline-…/` nor `boards/` in it exists yet.
    let root = temp_root("first-open");
    let data = root.parent().unwrap().to_path_buf();
    let faults = Arc::new(Faults::default());
    let handle = service::start_with(
        root.clone(),
        INSTANCE.to_owned(),
        StoreLimits::default(),
        Box::new(FaultIo(Arc::clone(&faults))),
        |_| {},
    )
    .unwrap();
    assert!(handle.opened());
    let call = TimelineCall::parse(
        "noa.timeline.registerRun",
        json!({ "requestId": "q", "publishPolicy": { "owner": "runner", "producerId": "r" } }),
    )
    .unwrap()
    .unwrap();
    let created: RegisterRunResult = ok(handle.call(call, Arc::new(Vec::new())));
    let syncs = faults.dir_syncs.lock().clone();
    // The directory of every new entry: `noa-timeline-…/` (in the temp
    // dir), `boards/`, the board, and the board's files.
    let board = root.join(&created.board_id);
    for dir in [data.parent().unwrap(), &data, &root, &board] {
        assert!(syncs.iter().any(|(d, _)| d == dir), "{dir:?}: {syncs:?}");
    }
    assert!(
        syncs
            .iter()
            .all(|(_, thread)| thread == service::THREAD_NAME),
        "{syncs:?}"
    );
}

#[test]
fn ac1_store_that_cannot_open_is_not_opened() {
    let base = temp_root("unopenable").parent().unwrap().to_path_buf();
    fs::create_dir_all(&base).unwrap();
    // A path through a file: the directory cannot be made.
    let file = base.join("not-a-dir");
    fs::write(&file, b"").unwrap();
    let handle = service::start(file.join("boards")).unwrap();
    assert!(!handle.opened());
    // The directory is made, but the store cannot finish opening it.
    let faults = Arc::new(Faults::default());
    faults.fail_dir_syncs.store(true, Ordering::SeqCst);
    let root = base.join("boards");
    let handle = service::start_with(
        root.clone(),
        INSTANCE.to_owned(),
        StoreLimits::default(),
        Box::new(FaultIo(faults)),
        |_| {},
    )
    .unwrap();
    assert!(!handle.opened());
    assert!(root.is_dir());
}

#[test]
fn ac1_key_b_duplicate_uses_up_its_request_id() {
    let root = temp_root("alias");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    let first: PostResult = ok(post(&mut store, manual(&b, "cli", "r1", input("t", "e1"))));
    let dup: PostResult = ok(post(&mut store, manual(&b, "cli", "r2", input("t", "e1"))));
    assert!(dup.duplicate);
    assert_eq!(dup.post_id, first.post_id);
    // r2 is used: another payload under it conflicts, the same is the same
    // post again.
    let other = manual(&b, "cli", "r2", input("other", "e2"));
    let err = post(&mut store, other.clone()).unwrap_err();
    assert_eq!(err.kind, TimelineErrorKind::Conflict);
    assert_eq!(
        err.existing_post_id.as_deref(),
        Some(first.post_id.as_str())
    );
    let again: PostResult = ok(post(&mut store, manual(&b, "cli", "r2", input("t", "e1"))));
    assert!(again.duplicate);
    assert_eq!(again.post_id, first.post_id);
    // Also after a restart.
    drop(store);
    let mut store = open(&root);
    assert_eq!(kind(post(&mut store, other)), TimelineErrorKind::Conflict);
    assert_eq!(list(&mut store, json!({ "boardId": b })).head_seq, Seq(1));
}

#[test]
fn ac1_correction_needs_a_new_event_id() {
    let root = temp_root("correction-event");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    let original: PostResult = ok(post(&mut store, manual(&b, "cli", "r0", input("t", "e0"))));
    let correction = |request: &str, event: &str| {
        let mut post = input("fixed", event);
        post["sourceRef"]["sessionId"] = json!("another-session");
        post["supersedesPostId"] = json!(original.post_id);
        manual(&b, "cli", request, post)
    };
    assert_eq!(
        kind(post(&mut store, correction("r1", "e0"))),
        TimelineErrorKind::InvalidParams
    );
    assert_eq!(list(&mut store, json!({ "boardId": b })).head_seq, Seq(1));
    let fixed: PostResult = ok(post(&mut store, correction("r2", "e1")));
    assert_eq!(fixed.seq, Seq(2));
}

#[test]
fn ac1_query_matches_body_by_unicode_lowercase() {
    let root = temp_root("query-unicode");
    let mut store = open(&root);
    let b = board(&mut store, "b").board_id;
    // U+212A KELVIN SIGN lowercases to `k`; U+0130 to `i` + U+0307.
    for (i, body) in ["\u{212A}ELVEN", "\u{130}STANBUL"].into_iter().enumerate() {
        let mut post = input(&format!("t{i}"), &format!("e{i}"));
        post["bodyMarkdown"] = json!(body);
        ok::<PostResult>(super::tests::post(
            &mut store,
            manual(&b, "cli", &format!("r{i}"), post),
        ));
    }
    let page = list(&mut store, json!({ "boardId": b, "query": "kel" }));
    assert_eq!(seqs(&page), [1]);
    let page = list(&mut store, json!({ "boardId": b, "query": "i" }));
    assert_eq!(seqs(&page), [2]);
}

#[test]
fn ac4_root_rebind_keeps_runs_another_instance_registered() {
    let root = temp_root("rebind-race");
    let live = panels(&[1]);
    let mut first = open(&root);
    let q1 = runner_run(&mut first, "q1", None);
    let faults = Arc::new(Faults::default());
    let mut second = open_faulty(&root, "instance-b", &faults);
    // After the second store read the board and before it locks it to
    // rebind, the first registers q2 there and exits.
    let hook_board = q1.board_id.clone();
    *faults.before_lock.lock() = Some(Box::new(move || {
        let mut first = first;
        runner_run(&mut first, "q2", Some(&hook_board));
    }));
    let mut again = json!({
        "requestId": "q1", "orchestratorPaneId": "1",
        "publishPolicy": { "owner": "runner", "producerId": "runner:t" },
    });
    let rebound: RegisterRunResult = ok(call(
        &mut second,
        "noa.timeline.registerRun",
        again.clone(),
        &live,
    ));
    assert!(!rebound.created);
    assert_eq!(rebound.run_id, q1.run_id);
    drop(second);
    // q2 is still registered, and pane 1 of the second instance is a root.
    let mut store = open_as(&root, "instance-b");
    let q2 = runner_run(&mut store, "q2", Some(&q1.board_id));
    assert!(!q2.created);
    let ctx = context(&mut store, json!({ "paneId": "1" }), &live);
    assert_eq!(ctx.board_id, q1.board_id);
    again["boardId"] = json!(q1.board_id);
    let replayed: RegisterRunResult =
        ok(call(&mut store, "noa.timeline.registerRun", again, &live));
    assert_eq!(replayed.run_id, q1.run_id);
}

#[test]
fn ac4_register_run_resend_finds_another_instances_run() {
    let root = temp_root("resend-other");
    // The second instance is up before the first registers anything.
    let faults = Arc::new(Faults::default());
    let mut second = open_faulty(&root, "instance-b", &faults);
    let mut first = open(&root);
    let q = runner_run(&mut first, "q", None);
    // The first still holds the board's lock: a re-send needs no lock.
    let again = runner_run(&mut second, "q", Some(&q.board_id));
    assert!(!again.created);
    assert_eq!(again.run_id, q.run_id);
    drop(first);
    // A board the second already read, registered on afterwards.
    let q3 = runner_run(&mut open(&root), "q3", Some(&q.board_id));
    assert!(q3.created);
    let again = runner_run(&mut second, "q3", Some(&q.board_id));
    assert!(!again.created);
    assert_eq!(again.run_id, q3.run_id);
    // Registered by the first between the second's read and its lock.
    let registered = Arc::new(parking_lot::Mutex::new(None));
    let (hook_root, hook_board, hook_out) = (root.clone(), q.board_id.clone(), registered.clone());
    *faults.before_lock.lock() = Some(Box::new(move || {
        let run = runner_run(&mut open(&hook_root), "q4", Some(&hook_board));
        *hook_out.lock() = Some(run);
    }));
    let again = runner_run(&mut second, "q4", Some(&q.board_id));
    let q4 = registered.lock().take().expect("the hook ran");
    assert!(!again.created);
    assert_eq!(again.run_id, q4.run_id);
}

#[test]
fn ac4_register_run_resend_after_panes_closed() {
    let root = temp_root("resend-closed");
    let mut store = open(&root);
    let live = panels(&[1, 2]);
    let first: RegisterRunResult = ok(register(
        &mut store,
        "q",
        Some(1),
        Some(2),
        json!({}),
        &live,
    ));
    for open_now in [panels(&[1]), Vec::new()] {
        let again: RegisterRunResult = ok(register(
            &mut store,
            "q",
            Some(1),
            Some(2),
            json!({}),
            &open_now,
        ));
        assert!(!again.created);
        assert_eq!(
            (&again.run_id, &again.worker_id),
            (&first.run_id, &first.worker_id)
        );
    }
    // A new request still needs its panes open.
    assert_eq!(
        kind(register(
            &mut store,
            "q-new",
            Some(1),
            Some(2),
            json!({}),
            &[]
        )),
        TimelineErrorKind::NotFound
    );
}

#[test]
fn ac4_run_order_is_registration_order() {
    let root = temp_root("run-order");
    let live = panels(&[1, 2]);
    let (board_id, a, b) = {
        let mut store = open(&root);
        let a: RegisterRunResult = ok(register(
            &mut store,
            "qa",
            Some(1),
            Some(2),
            json!({}),
            &live,
        ));
        let b: RegisterRunResult = ok(register(
            &mut store,
            "qb",
            Some(1),
            Some(2),
            json!({}),
            &live,
        ));
        (a.board_id, a.run_id, b.run_id)
    };
    let meta_path = root.join(&board_id).join("meta.json");
    // The clock went back between the two registrations, then both fall in
    // one millisecond: neither createdAt nor the run ids decide the order.
    for (at_a, at_b) in [
        ("2026-01-01T00:00:01.000Z", "2026-01-01T00:00:00.000Z"),
        ("2026-01-01T00:00:00.000Z", "2026-01-01T00:00:00.000Z"),
    ] {
        let mut meta: Value = serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
        meta["runs"][&a]["info"]["createdAt"] = json!(at_a);
        meta["runs"][&b]["info"]["createdAt"] = json!(at_b);
        fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
        let mut store = open(&root);
        let me = context(&mut store, json!({ "paneId": "2" }), &live)
            .me
            .unwrap();
        let ids: Vec<&str> = me.runs.iter().map(|run| run.run_id.as_str()).collect();
        assert_eq!(ids, [a.as_str(), b.as_str()], "{at_a} {at_b}");
    }
}

fn listed_ids(store: &mut Store) -> Vec<String> {
    let listing: BoardsResult = ok(call(store, "noa.timeline.boards", json!({}), &[]));
    let mut ids: Vec<String> = listing.boards.into_iter().map(|b| b.board_id).collect();
    ids.sort();
    ids
}

/// Finding B: a board directory removed by hand while Noa runs (as the
/// `storage_full` message suggests) drops out of the listing, and its reads
/// answer `not_found`, both for a board this store reads only and for one
/// it holds the write lock of.
#[test]
fn board_deleted_by_hand_drops_out_and_reads_not_found() {
    let root = temp_root("deleted-board");
    let (read_only, written, post_id) = {
        let mut store = open(&root);
        let read_only = board(&mut store, "a").board_id;
        let written = board(&mut store, "b").board_id;
        let posted: PostResult = ok(post(
            &mut store,
            manual(&read_only, "cli", "r", input("t", "e")),
        ));
        (read_only, written, posted.post_id)
    };
    // Reopened: neither board is locked by this store yet.
    let mut store = open(&root);
    let mut both = vec![read_only.clone(), written.clone()];
    both.sort();
    assert_eq!(listed_ids(&mut store), both);
    // `written` is now locked by this store (it holds its writer).
    ok::<PostResult>(post(
        &mut store,
        manual(&written, "cli", "r", input("t", "e")),
    ));

    fs::remove_dir_all(root.join(&read_only)).unwrap();
    assert_eq!(listed_ids(&mut store), vec![written.clone()]);
    let get = call(
        &mut store,
        "noa.timeline.get",
        json!({ "boardId": read_only, "postId": post_id }),
        &[],
    );
    assert_eq!(kind(get), TimelineErrorKind::NotFound);

    fs::remove_dir_all(root.join(&written)).unwrap();
    assert_eq!(
        kind(call(
            &mut store,
            "noa.timeline.list",
            json!({ "boardId": written }),
            &[],
        )),
        TimelineErrorKind::NotFound
    );
    assert!(listed_ids(&mut store).is_empty());
}

/// Finding B: one board whose `meta.json` stopped parsing is left out of
/// the listing (as `scan` leaves out one it cannot load) instead of failing
/// the whole call.
#[test]
fn boards_listing_skips_an_unreadable_board() {
    let root = temp_root("unreadable-board");
    let (bad, good) = {
        let mut store = open(&root);
        (
            board(&mut store, "a").board_id,
            board(&mut store, "b").board_id,
        )
    };
    let mut store = open(&root);
    assert_eq!(listed_ids(&mut store).len(), 2);
    fs::write(root.join(&bad).join("meta.json"), b"{ not json").unwrap();
    assert_eq!(listed_ids(&mut store), vec![good]);
}
