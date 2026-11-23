//! Regression tests for the review of the closed workers (2026-11-24).

use std::collections::HashSet;

use super::*;
use crate::live::journal::{ARCHIVE_READ_BYTES, Archive, Journal, gone, records};

const NOW: f64 = 1_000_000.0;

fn line(value: serde_json::Value) -> String {
    let mut text = value.to_string();
    text.push('\n');
    text
}

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "agent-dashboard-review-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// This instance's journal: an open orchestrator's (`read_journals`).
fn own(name: &str, text: &str) -> Journal {
    Journal::parsed(name.to_owned(), None, text).of_this_instance()
}

/// Review #6: a pane started 3 s after the journal's spawn, but under
/// another open spawner, is not the journal's worker.
#[test]
fn a_known_other_spawner_is_another_worker_whatever_the_time() {
    let at = NOW as u64 - 600;
    let text = line(serde_json::json!({ "ev": "spawn", "ts": at, "pane": 4, "label": "a's" }));
    let journals = [own("s-a", &text)];
    let panels = super::panels(vec![
        panel(serde_json::json!({ "paneId": "2", "surfaceId": "s-b" })),
        panel(serde_json::json!({
            "paneId": "4", "parentPaneId": "2",
            "agent": { "seq": 1, "kind": "codex", "state": "running" },
            "spawn": { "argv": ["codex"], "startedAt": at + 3 },
        })),
    ]);
    let vanished = HashMap::from([(4, (NOW - 10.0, Some(at - 1)))]);
    let gone = gone(&journals, &panels, &vanished, NOW);
    assert_eq!(gone.len(), 1, "{gone:?}");
    assert_eq!(gone[0].record.label.as_deref(), Some("a's"));
}

/// Review #7: a journal of another Noa in the same data directory (or
/// from before a restart) counts only what it says closed.
#[test]
fn an_archived_record_without_a_close_is_not_counted_as_closed() {
    let at = NOW as u64 - 600;
    let text = [
        line(serde_json::json!({ "ev": "spawn", "ts": at, "pane": 4, "label": "elsewhere" })),
        line(serde_json::json!({ "ev": "spawn", "ts": at, "pane": 5, "label": "closed" })),
        line(serde_json::json!({ "ev": "close", "ts": at + 60, "pane": 5 })),
    ]
    .concat();
    let journals = [Journal::parsed("s-other".into(), None, &text)];
    let gone = gone(&journals, &HashMap::new(), &HashMap::new(), NOW);
    let labels: Vec<_> = gone.iter().map(|g| g.record.label.as_deref()).collect();
    assert_eq!(labels, [Some("closed")]);
}

/// Review #8: a worker whose last outcome was an error stays failed once
/// its pane is closed.
#[test]
fn a_closed_worker_that_failed_stays_failed() {
    let at = NOW as u64 - 600;
    let text = [
        line(serde_json::json!({ "ev": "spawn", "ts": at, "pane": 4, "label": "broke" })),
        line(serde_json::json!({ "ev": "outcome", "ts": at + 30, "pane": 4, "reason": "error" })),
        line(serde_json::json!({ "ev": "close", "ts": at + 60, "pane": 4 })),
    ]
    .concat();
    let mut tracker = Tracker::default();
    tracker.gone = gone(&[own("s", &text)], &tracker.panels, &tracker.vanished, NOW);
    let fleet = tracker.fleet(NOW, None);
    let worker = &fleet.workers[0];
    assert_eq!(worker.state, State::Error);
    assert_eq!(
        worker.need(),
        None,
        "a closed failure asks nobody for anything"
    );
}

/// Review #9: a second `close` of a pane already gone keeps the first time.
#[test]
fn the_first_close_is_when_it_closed() {
    let text = [
        line(serde_json::json!({ "ev": "spawn", "ts": 10, "pane": 4 })),
        line(serde_json::json!({ "ev": "close", "ts": 20, "pane": 4, "worktree": null })),
        line(serde_json::json!({ "ev": "close", "ts": 3620, "pane": 4,
            "worktree": { "removed": true, "result": "/main/.noa/result-x.md" } })),
    ]
    .concat();
    let records = records(&text);
    assert_eq!(records[0].closed_at, Some(20));
    assert_eq!(
        records[0].moved_to.as_deref(),
        Some(Path::new("/main/.noa/result-x.md"))
    );
}

/// Review #10: another spawner's prune moved this worker's result; the
/// move names no pane, only where the file came from.
#[test]
fn a_result_moved_by_another_spawners_prune_is_found_where_it_went() {
    let root = dir("moved");
    let wt = root.join("wt");
    let main = root.join("main");
    std::fs::create_dir_all(main.join(".noa")).unwrap();
    let moved = main.join(".noa/result-y.md");
    std::fs::write(&moved, "y's report").unwrap();
    let at = NOW as u64 - 600;
    let a = line(
        serde_json::json!({ "ev": "close", "ts": at + 50, "pane": 3, "worktree": {
            "removed": true,
            "moved": [{ "from": wt.join(".noa/result-y.md"), "to": moved }],
        }}),
    );
    let b = [
        line(
            serde_json::json!({ "ev": "spawn", "ts": at, "pane": 5, "label": "y",
            "cwd": wt, "result": ".noa/result-y.md" }),
        ),
        line(serde_json::json!({ "ev": "close", "ts": at + 40, "pane": 5 })),
    ]
    .concat();
    let journals = [own("s-a", &a), own("s-b", &b)];
    let gone = gone(&journals, &HashMap::new(), &HashMap::new(), NOW);
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0].result.as_deref(), Some(moved.as_path()));
}

/// Review #11: a closed grandchild hangs where it hung while open: under
/// the root A, its parent B.
#[test]
fn a_closed_grandchild_keeps_its_root_and_parent() {
    let at = NOW as u64 - 600;
    let text = [
        line(serde_json::json!({ "ev": "spawn", "ts": at, "pane": 3, "label": "c" })),
        line(serde_json::json!({ "ev": "close", "ts": at + 60, "pane": 3 })),
    ]
    .concat();
    let panels = super::panels(vec![
        panel(serde_json::json!({ "paneId": "1", "label": "lead", "surfaceId": "s-a" })),
        panel(serde_json::json!({
            "paneId": "2", "parentPaneId": "1", "label": "b", "surfaceId": "s-b",
            "agent": { "seq": 1, "kind": "claude", "state": "running" },
        })),
    ]);
    let mut tracker = Tracker::default();
    tracker.merge(panels.into_values().collect(), NOW);
    tracker.gone = gone(
        &[own("s-b", &text)],
        &tracker.panels,
        &tracker.vanished,
        NOW,
    );
    let fleet = tracker.fleet(NOW, None);
    let c = fleet.workers.iter().find(|w| w.is_closed()).unwrap();
    assert_eq!(c.orchestrator, "lead");
    assert_eq!(c.orchestrator_pane, Some(1));
    assert_eq!(c.parent, Some(2));
}

/// Review #13: a line that starts exactly where the read starts is whole.
#[test]
fn a_line_starting_at_the_read_boundary_is_kept() {
    let dir = dir("boundary");
    let at = unix_now() as u64 - 100;
    let head = line(serde_json::json!({ "ev": "outcome", "ts": at, "pane": 999 }));
    let spawn = line(serde_json::json!({ "ev": "spawn", "ts": at, "pane": 4, "label": "edge" }));
    let close = line(serde_json::json!({ "ev": "close", "ts": at + 1, "pane": 4 }));
    // Pads the read part to exactly ARCHIVE_READ_BYTES.
    let used = (spawn.len() + close.len()) as u64;
    let pad_line = |n: usize| line(serde_json::json!({ "ev": "x", "pad": "p".repeat(n) }));
    let overhead = pad_line(0).len() as u64;
    let pad = pad_line((ARCHIVE_READ_BYTES - used - overhead) as usize);
    let text = [head, spawn, close, pad].concat();
    assert_eq!(
        text.len() as u64 - ARCHIVE_READ_BYTES,
        text.find("{\"ev\":\"spawn\"").unwrap() as u64
    );
    std::fs::write(dir.join("0xedge.jsonl"), &text).unwrap();
    let mut archive = Archive::at(Some(dir.clone()));
    archive.scan(unix_now(), &HashSet::new());
    std::fs::remove_dir_all(&dir).unwrap();
    let journal = archive.journals.values().next().unwrap();
    let labels: Vec<_> = journal.records.iter().map(|r| r.label.as_deref()).collect();
    assert_eq!(labels, [Some("edge")]);
}
