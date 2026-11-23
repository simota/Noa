//! Workers rebuilt from the spawners' journals once their panes are gone.

use std::collections::HashSet;
use std::os::unix::fs::symlink;
use std::time::{Duration, SystemTime};

use super::*;
use crate::live::journal::{
    ARCHIVE_FILES, ARCHIVE_READ_BYTES, ARCHIVE_SCAN_SECS, Archive, CLOSED_MAX, CLOSED_MAX_AGE_SECS,
    Journal, file_name, gone, records,
};

const NOW: f64 = 1_000_000.0;

fn line(value: serde_json::Value) -> String {
    let mut text = value.to_string();
    text.push('\n');
    text
}

fn spawn(pane: u64, ts: u64, label: &str) -> String {
    line(serde_json::json!({
        "ev": "spawn", "ts": ts, "pane": pane, "label": label,
        "argv": ["codex", "-m", "gpt-x"], "cwd": "/r", "branch": format!("orch/{label}"),
        "result": format!(".noa/result-{label}.md"),
    }))
}

fn close(pane: u64, ts: u64) -> String {
    line(serde_json::json!({ "ev": "close", "ts": ts, "pane": pane, "worktree": null }))
}

fn task(pane: u64, ts: u64, text: &str) -> String {
    line(serde_json::json!({ "ev": "send", "ts": ts, "pane": pane, "text": text, "answer": false }))
}

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "agent-dashboard-journal-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An open orchestrator's journal: of the Noa the feed follows.
fn journal(name: &str, text: &str) -> Journal {
    Journal::parsed(name.to_owned(), None, text).of_this_instance()
}

/// A worker pane spawned by 1, started at `started_at`.
fn open_worker(pane: &str, started_at: u64) -> Panel {
    panel(serde_json::json!({
        "paneId": pane,
        "parentPaneId": "1",
        "agent": { "seq": 1, "kind": "codex", "state": "running" },
        "spawn": { "argv": ["codex"], "startedAt": started_at },
    }))
}

#[test]
fn a_journal_records_each_spawn_with_what_followed_it() {
    let text = [
        spawn(4, 10, "fix"),
        task(4, 11, "\n  first line\nsecond"),
        line(serde_json::json!({ "ev": "send", "ts": 12, "pane": 4, "text": "1", "answer": true })),
        line(serde_json::json!({ "ev": "outcome", "ts": 13, "pane": 4, "reason": "finished" })),
        "not json\n".to_owned(),
        close(4, 20),
        // Noa restarted and gave pane 4 out again.
        spawn(4, 500, "again"),
        // A close that names a pane this journal never spawned.
        close(99, 501),
    ]
    .concat();
    let records = records(&text);
    assert_eq!(records.len(), 2);
    let first = &records[0];
    assert_eq!(first.pane, 4);
    assert_eq!(first.spawned_at, 10);
    assert_eq!(first.task, "first line");
    assert_eq!(first.outcome.as_deref(), Some("finished"));
    assert_eq!(first.closed_at, Some(20));
    assert_eq!(first.argv, ["codex", "-m", "gpt-x"]);
    let again = &records[1];
    assert_eq!(again.label.as_deref(), Some("again"));
    assert_eq!(again.task, "");
    assert_eq!(again.closed_at, None);
}

#[test]
fn a_pruned_close_moves_the_result() {
    let text = [
        spawn(4, 10, "fix"),
        spawn(5, 10, "y"),
        line(
            serde_json::json!({ "ev": "close", "ts": 20, "pane": 4, "worktree": {
                "removed": true, "result": "/main/.noa/result-fix.md",
                "moved": [{ "paneId": "5", "to": "/main/.noa/result-y.md" }],
            }}),
        ),
    ]
    .concat();
    let records = records(&text);
    assert_eq!(
        records[0].moved_to.as_deref(),
        Some(Path::new("/main/.noa/result-fix.md"))
    );
    assert_eq!(
        records[1].moved_to.as_deref(),
        Some(Path::new("/main/.noa/result-y.md"))
    );
}

#[test]
fn a_closed_or_vanished_worker_is_gone_and_an_open_one_is_not() {
    let now = NOW;
    let at = now as u64 - 600;
    let text = [
        spawn(4, at, "closed"),
        close(4, at + 300),
        spawn(5, at, "vanished"),
        spawn(6, at, "open"),
    ]
    .concat();
    let journals = [journal("s1", &text)];
    let orchestrator =
        panel(serde_json::json!({ "paneId": "1", "label": "lead", "surfaceId": "s1" }));
    let panels = super::panels(vec![orchestrator, open_worker("6", at - 1)]);
    let vanished = HashMap::from([(5, (now - 60.0, None))]);
    let gone = gone(&journals, &panels, &vanished, now);
    let ends: Vec<(u64, Option<f64>)> = gone.iter().map(|g| (g.record.pane, g.ended_at)).collect();
    // The newest end first.
    assert_eq!(ends, [(5, Some(now - 60.0)), (4, Some((at + 300) as f64))]);
    assert!(
        gone.iter()
            .all(|g| g.orchestrator == "lead" && g.orchestrator_pane == Some(1))
    );
    assert!(
        gone.iter().all(|g| g.key >= 1 << 63),
        "a key is never a pane id"
    );
}

/// After Noa restarted, pane 4 is another pane: started long after the
/// journal's spawn. The journal's worker is gone, the new pane stays live,
/// and the two are two workers.
#[test]
fn a_pane_id_given_out_again_is_another_worker() {
    let at = NOW as u64 - 3600;
    let text = spawn(4, at, "old");
    let journals = [journal("s-old", &text)];
    let mut tracker = Tracker::default();
    tracker.merge(
        vec![
            panel(serde_json::json!({ "paneId": "1", "surfaceId": "s-new" })),
            open_worker("4", at + 1800),
        ],
        NOW,
    );
    tracker.gone = gone(&journals, &tracker.panels, &tracker.vanished, NOW);
    let fleet = tracker.fleet(NOW, None);
    assert_eq!(fleet.workers.len(), 2, "{:?}", fleet.workers);
    let live = fleet.worker(4).expect("the new pane");
    assert!(!live.is_closed());
    let old = fleet.workers.iter().find(|w| w.is_closed()).unwrap();
    assert_eq!(old.closed_pane, Some(4));
    assert_eq!(old.label, "old");
    assert_eq!(old.orchestrator, "閉じた親 -old", "its spawner is not open");

    // No spawner known for the open pane: its start decides.
    let mut tracker = Tracker::default();
    tracker.merge(vec![open_worker("4", at + 1800)], NOW);
    assert_eq!(
        gone(&journals, &tracker.panels, &tracker.vanished, NOW).len(),
        1
    );

    // The same spawn, started a moment before it was journaled: one worker.
    let mut tracker = Tracker::default();
    tracker.merge(vec![open_worker("4", at - 3)], NOW);
    assert!(gone(&journals, &tracker.panels, &tracker.vanished, NOW).is_empty());

    // Two spawns under the same id in two journals: two closed workers.
    let other = journal("s-other", &spawn(4, at + 10, "other"));
    let both = [journal("s-old", &text), other];
    let gone = gone(&both, &HashMap::new(), &HashMap::new(), NOW);
    assert_eq!(gone.len(), 2);
    assert_ne!(gone[0].key, gone[1].key);
}

/// Review #14: the age limit on its own, with fewer records than the cap.
/// A record's age is from its close, else its spawn.
#[test]
fn only_the_last_day_is_shown() {
    let now = NOW as u64;
    let day = CLOSED_MAX_AGE_SECS as u64;
    let text = [
        spawn(1, now - day - 100, "closed-inside"),
        close(1, now - day + 1),
        spawn(2, now - day - 100, "closed-outside"),
        close(2, now - day - 1),
        spawn(3, now - day - 1, "spawned-outside"),
        spawn(4, now - day + 1, "spawned-inside"),
        spawn(5, now - 5 * day, "long-run-closed-now"),
        close(5, now - 10),
    ]
    .concat();
    let gone = gone(
        &[journal("s", &text)],
        &HashMap::new(),
        &HashMap::new(),
        NOW,
    );
    let mut labels: Vec<&str> = gone
        .iter()
        .filter_map(|g| g.record.label.as_deref())
        .collect();
    labels.sort_unstable();
    assert_eq!(
        labels,
        ["closed-inside", "long-run-closed-now", "spawned-inside"]
    );
}

#[test]
fn at_most_the_newest_fifty_are_shown() {
    let now = NOW as u64;
    let mut text = String::new();
    for pane in 1..=(CLOSED_MAX as u64 + 10) {
        text.push_str(&spawn(pane, now - 1000 + pane, &format!("w{pane}")));
    }
    let gone = gone(
        &[journal("s", &text)],
        &HashMap::new(),
        &HashMap::new(),
        NOW,
    );
    assert_eq!(gone.len(), CLOSED_MAX);
    assert_eq!(
        gone[0].record.pane,
        CLOSED_MAX as u64 + 10,
        "the newest first"
    );
    assert_eq!(gone[CLOSED_MAX - 1].record.pane, 11, "the oldest kept");
}

#[test]
fn a_closed_workers_result_is_read_only_from_a_regular_file() {
    let cwd = dir("results");
    std::fs::create_dir_all(cwd.join(".noa")).unwrap();
    std::fs::write(cwd.join(".noa/result-full.md"), "done").unwrap();
    std::fs::write(cwd.join(".noa/result-empty.md"), "").unwrap();
    std::fs::write(cwd.join("elsewhere.md"), "secret").unwrap();
    symlink(cwd.join("elsewhere.md"), cwd.join(".noa/result-link.md")).unwrap();
    let at = NOW as u64 - 100;
    let spawn_in = |pane: u64, label: &str| {
        line(serde_json::json!({
            "ev": "spawn", "ts": at, "pane": pane, "label": label,
            "cwd": cwd.to_str().unwrap(), "result": format!(".noa/result-{label}.md"),
        }))
    };
    let text = [
        spawn_in(1, "full"),
        spawn_in(2, "empty"),
        spawn_in(3, "link"),
    ]
    .concat();
    let journals = [journal("s", &text)];
    let gone = gone(&journals, &HashMap::new(), &HashMap::new(), NOW);
    let results: HashMap<u64, Option<PathBuf>> = gone
        .iter()
        .map(|g| (g.record.pane, g.result.clone()))
        .collect();
    std::fs::remove_dir_all(&cwd).unwrap();
    assert_eq!(results[&1], Some(cwd.join(".noa/result-full.md")));
    assert_eq!(results[&2], None);
    assert_eq!(results[&3], None);
}

#[test]
fn a_closed_worker_is_done_with_its_times_from_spawn_and_close() {
    let at = NOW as u64 - 1800;
    let text = [
        spawn(9, at, "bench"),
        task(9, at + 1, "measure"),
        close(9, at + 600),
    ]
    .concat();
    let journals = [journal("s", &text)];
    let mut tracker = Tracker::default();
    tracker.gone = gone(&journals, &tracker.panels, &tracker.vanished, NOW);
    let fleet = tracker.fleet(NOW, None);
    let worker = &fleet.workers[0];
    assert_eq!(worker.closed_pane, Some(9));
    assert_eq!(worker.state, State::Finished);
    assert_eq!(worker.need(), None, "nothing to do for it");
    assert!(!worker.can_close_unseen());
    assert_eq!(worker.kind, "codex");
    assert_eq!(worker.model, "gpt-x");
    assert_eq!(worker.branch, "orch/bench");
    assert_eq!(worker.task, "measure");
    assert_eq!(worker.started_minutes, Some(30.0));
    assert_eq!(worker.took_minutes, Some(10.0));
    assert_eq!(worker.since, "20 分前に閉じた");
    assert_eq!(
        worker.phases,
        [(Phase::Running, 10.0), (Phase::Ended, 20.0)]
    );
    assert_eq!(fleet.orchestrators.len(), 1);
}

fn write_aged(path: &Path, text: &str, age: Duration) {
    std::fs::write(path, text).unwrap();
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - age).unwrap();
}

fn unix_seconds() -> f64 {
    unix_now()
}

#[test]
fn the_archive_reads_recent_regular_journals_only() {
    let dir = dir("archive");
    let now = unix_seconds();
    let at = now as u64 - 100;
    write_aged(
        &dir.join("0xa1.jsonl"),
        &spawn(4, at, "recent"),
        Duration::ZERO,
    );
    write_aged(
        &dir.join("0xa2.jsonl"),
        &spawn(5, at, "stale"),
        Duration::from_secs(90_000),
    );
    std::fs::write(dir.join("0xa1.jsonl.lock"), "").unwrap();
    std::fs::write(dir.join("other.txt"), spawn(6, at, "txt")).unwrap();
    let outside = dir.join("outside.json");
    std::fs::write(&outside, spawn(7, at, "linked")).unwrap();
    symlink(&outside, dir.join("0xa3.jsonl")).unwrap();
    // Read already as an open orchestrator's journal.
    std::fs::write(dir.join("0xa4.jsonl"), spawn(8, at, "open")).unwrap();

    let mut archive = Archive::at(Some(dir.clone()));
    archive.scan(now, &HashSet::from(["0xa4".to_owned()]));
    let names: Vec<&str> = archive.journals.values().map(|j| j.name.as_str()).collect();
    assert_eq!(names, ["0xa1"]);

    // Looked through again only after ARCHIVE_SCAN_SECS, and a file read
    // again only when it changed.
    std::fs::write(
        dir.join("0xa1.jsonl"),
        [spawn(4, at, "recent"), close(4, at + 5)].concat(),
    )
    .unwrap();
    archive.scan(now + 1.0, &HashSet::new());
    let records = |archive: &Archive| archive.journals.values().next().unwrap().records.clone();
    assert_eq!(records(&archive)[0].closed_at, None);
    archive.scan(
        now + ARCHIVE_SCAN_SECS + 1.0,
        &HashSet::from(["0xa4".to_owned()]),
    );
    assert_eq!(records(&archive)[0].closed_at, Some(at + 5));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_archive_reads_the_newest_journals_and_their_ends_only() {
    let dir = dir("bounds");
    let now = unix_seconds();
    let at = now as u64 - 100;
    for n in 0..ARCHIVE_FILES + 3 {
        let age = Duration::from_secs(60 * (n as u64 + 1));
        write_aged(
            &dir.join(format!("0x{n:02}.jsonl")),
            &spawn(n as u64 + 1, at, "w"),
            age,
        );
    }
    // A journal longer than ARCHIVE_READ_BYTES: its first spawn is not read.
    let filler = line(
        serde_json::json!({ "ev": "outcome", "ts": at, "pane": 999, "reason": "x".repeat(200) }),
    );
    let mut long = spawn(1, at, "early");
    while (long.len() as u64) < ARCHIVE_READ_BYTES + 4096 {
        long.push_str(&filler);
    }
    long.push_str(&spawn(2, at, "late"));
    write_aged(&dir.join("0xlong.jsonl"), &long, Duration::ZERO);

    let mut archive = Archive::at(Some(dir.clone()));
    archive.scan(now, &HashSet::new());
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(archive.journals.len(), ARCHIVE_FILES);
    let long = archive
        .journals
        .values()
        .find(|j| j.name == "0xlong")
        .expect("the newest");
    let labels: Vec<_> = long.records.iter().map(|r| r.label.as_deref()).collect();
    assert_eq!(labels, [Some("late")]);
    assert!(
        !archive.journals.values().any(|j| j.name == "0x34"),
        "the oldest is left out"
    );
}

#[test]
fn a_journal_file_is_named_after_the_surface_as_ctl_names_it() {
    assert_eq!(file_name("0x00000000000000a3"), "0x00000000000000a3");
    assert_eq!(file_name("../x/y"), "___x_y");
}

/// The feed finds a closed orchestrator's journal in the directory and
/// sends its workers with the live ones; the same journal outlives a Noa
/// restart (another instance), being a file.
#[test]
fn the_feed_sends_the_closed_workers_of_the_journal_directory() {
    let dir = dir("feed");
    let at = unix_seconds() as u64 - 300;
    std::fs::write(
        dir.join("0xdead.jsonl"),
        [spawn(7, at, "finished-earlier"), close(7, at + 120)].concat(),
    )
    .unwrap();
    let clock = TestClock::new();
    let (mut feed, published, _commands) = held_feed(&clock);
    feed.tracker.archive = Archive::at(Some(dir.clone()));
    let mut noa = ScriptedNoa::showing("a", "$ ready", &clock);
    assert!(matches!(feed.follow(&mut noa), Err(Stop::Lost(_))));
    let last = published.try_iter().last().expect("a snapshot");
    let closed: Vec<&str> = last
        .fleet
        .workers
        .iter()
        .filter(|w| w.is_closed())
        .map(|w| w.label.as_str())
        .collect();
    assert_eq!(closed, ["finished-earlier"]);
    assert!(last.fleet.worker(2).is_some_and(|w| !w.is_closed()));

    let mut restarted = ScriptedNoa::showing("b", "$ other", &clock);
    assert!(matches!(feed.follow(&mut restarted), Err(Stop::Lost(_))));
    let last = published.try_iter().last().expect("a snapshot");
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
        last.fleet.workers.iter().filter(|w| w.is_closed()).count(),
        1
    );
}

/// A pane the dashboard closed (no `close` in the journal) is gone with the
/// time the feed saw it leave.
#[test]
fn a_pane_seen_leaving_ends_at_that_time() {
    let clock = TestClock::new();
    let (mut feed, published, commands) = held_feed(&clock);
    let mut noa = ScriptedNoa::showing("a", "$ done", &clock);
    let at = unix_seconds() as u64 - 60;
    noa.panels[0].surface_id = Some("s-lead".into());
    noa.panels[1] = open_worker("2", at - 1);
    feed.serve("a");
    feed.tracker.journals.insert(
        "s-lead".into(),
        Journal::parsed(file_name("s-lead"), None, &spawn(2, at, "a")),
    );
    commands
        .send(Command::Close {
            instance: "a".into(),
            pane: 2,
        })
        .unwrap();
    assert!(matches!(feed.follow(&mut noa), Err(Stop::Lost(_))));
    assert_eq!(noa.closed, [2]);
    let last = published.try_iter().last().expect("a snapshot");
    let worker = last
        .fleet
        .workers
        .iter()
        .find(|w| w.is_closed())
        .expect("closed");
    assert_eq!(worker.closed_pane, Some(2));
    // It left between the spawn and now: not at the spawn.
    let took = worker.took_minutes.expect("its end is known") * 60.0;
    let upper = unix_seconds() - at as f64;
    assert!(took > 30.0 && took <= upper, "{took} s, at most {upper} s");
}

/// Review #16: the end is the time `replace` saw the pane leave, to the
/// second, and the times follow from it.
#[test]
fn the_end_of_a_pane_seen_leaving_is_when_it_left() {
    let started = 10_000;
    let spawned = started + 2;
    let left = 10_600.0;
    let lead = panel(serde_json::json!({ "paneId": "1", "surfaceId": "s-lead" }));
    let mut tracker = Tracker::default();
    tracker.replace(vec![lead.clone(), open_worker("2", started)], 10_100.0);
    tracker.replace(vec![lead], left);
    assert_eq!(tracker.vanished[&2], (left, Some(started)));
    let journals = [journal(&file_name("s-lead"), &spawn(2, spawned, "a"))];
    let now = left + 300.0;
    tracker.gone = gone(&journals, &tracker.panels, &tracker.vanished, now);
    let fleet = tracker.fleet(now, None);
    let worker = fleet.workers.iter().find(|w| w.is_closed()).unwrap();
    assert_eq!(tracker.gone[0].ended_at, Some(left));
    assert_eq!(worker.took_minutes, Some((left - spawned as f64) / 60.0));
    assert_eq!(worker.since, "5 分前に閉じた");
    assert_eq!(
        worker.phases,
        [
            (Phase::Running, (left - spawned as f64) / 60.0),
            (Phase::Ended, 5.0)
        ]
    );
}
