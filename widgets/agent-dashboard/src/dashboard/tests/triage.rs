use std::path::PathBuf;

use crate::fleet::{State, Tier};

use super::*;

/// The texts the list holds, its toolbar's included.
fn list_texts(dashboard: &Dashboard) -> Vec<String> {
    let list = dashboard.registry.list.as_ref().unwrap().list;
    let panel = dashboard.tree.parent(list).unwrap();
    texts_under(&dashboard.tree, panel)
}

/// dashboard-worker-list AC2: a notice or a quiet worker needs no
/// attention: 要対応 has no row for it, and a quiet one runs in 現在.
#[test]
fn what_only_informs_never_needs_attention() {
    let mut fleet = fleet::canned();
    fleet
        .workers
        .retain(|w| w.need().is_none_or(|n| n.tier() == Tier::Info));
    fleet.workers[0].attention = true;
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    assert_eq!(segment_count(&dashboard, Scope::Attention), 0);
    assert!(rows_of(&dashboard).contains(&13), "the quiet 13 runs");
    in_scope(&mut dashboard, &mut text, Scope::Attention);
    assert!(rows_of(&dashboard).is_empty());
    assert!(list_texts(&dashboard).contains(&"要対応はありません".to_owned()));
}

/// A file under the temporary directory, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, contents: &[u8]) -> Self {
        let path =
            std::env::temp_dir().join(format!("agent-dashboard-{}-{name}", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        TempFile(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The canned fleet on a live link, `docs-sync` (17, done) leaving `result`;
/// its drawer open.
fn drawer_on_result(result: PathBuf, can_focus: bool) -> (Dashboard, TextStore) {
    let mut fleet = fleet::canned();
    let done = fleet.workers.iter_mut().find(|w| w.pane == 17).unwrap();
    done.result = Some(result);
    let mut dashboard = Dashboard::of_snapshot(
        Grouping::Orchestrator,
        Snapshot {
            can_focus,
            ..live(fleet, 0)
        },
    );
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    dashboard.set_detail(Some(17), &mut text);
    dashboard.prepare(window(), &mut text);
    (dashboard, text)
}

fn drawer_texts(dashboard: &Dashboard) -> Vec<String> {
    let (root, _) = dashboard.registry.drawer.unwrap();
    labels_under(&dashboard.tree, root)
}

fn toggle_result(dashboard: &mut Dashboard, text: &mut TextStore) {
    let toggle = dashboard.registry.result_toggle.expect("a result button");
    let pos = center(&dashboard.tree, toggle);
    click(dashboard, text, pos);
}

#[test]
fn the_drawer_reads_the_result_only_when_asked() {
    let file = TempFile::new("result.md", b"# Review\nall green\n");
    let (mut dashboard, mut text) = drawer_on_result(file.0.clone(), true);
    assert!(!drawer_texts(&dashboard).contains(&"all green".to_owned()));

    toggle_result(&mut dashboard, &mut text);
    assert_eq!(dashboard.view.result, Some(17));
    let texts = drawer_texts(&dashboard);
    assert!(texts.contains(&"# Review".to_owned()), "{texts:?}");
    assert!(texts.contains(&"all green".to_owned()), "{texts:?}");

    toggle_result(&mut dashboard, &mut text);
    assert_eq!(dashboard.view.result, None);
    assert!(!drawer_texts(&dashboard).contains(&"all green".to_owned()));

    // Another worker's drawer shows no result read for 17.
    toggle_result(&mut dashboard, &mut text);
    dashboard.set_detail(Some(18), &mut text);
    assert!(!drawer_texts(&dashboard).contains(&"all green".to_owned()));
}

#[test]
fn a_result_that_is_not_a_regular_file_is_not_read() {
    let dir = std::env::temp_dir();
    let (mut dashboard, mut text) = drawer_on_result(dir, true);
    toggle_result(&mut dashboard, &mut text);
    let texts = drawer_texts(&dashboard);
    let reason = "読めません: 通常のファイルではありません".to_owned();
    assert!(texts.contains(&reason), "{texts:?}");
}

#[test]
fn a_long_result_is_cut() {
    let line = "x".repeat(99);
    let long = format!("{line}\n").repeat(400);
    let file = TempFile::new("long.md", long.as_bytes());
    let (text, cut) = drawer::read_result(&file.0).unwrap();
    assert!(cut);
    assert!(text.len() as u64 <= drawer::RESULT_MAX_BYTES);
    assert!(text.lines().count() <= drawer::RESULT_MAX_LINES);
    let short = TempFile::new("short.md", b"done\n");
    assert_eq!(
        drawer::read_result(&short.0),
        Ok(("done".to_owned(), false))
    );
}

#[test]
fn the_drawer_names_the_pane_and_what_opening_it_needs() {
    let file = TempFile::new("scopes.md", b"ok\n");
    let (dashboard, _text) = drawer_on_result(file.0.clone(), false);
    let texts = drawer_texts(&dashboard);
    assert!(
        texts.iter().any(|t| t.starts_with("ID 17 · 親 ")),
        "{texts:?}"
    );
    assert!(
        texts.contains(&drawer::NEEDS_CONTROL.to_owned()),
        "{texts:?}"
    );

    let (dashboard, _text) = drawer_on_result(file.0.clone(), true);
    assert!(!drawer_texts(&dashboard).contains(&drawer::NEEDS_CONTROL.to_owned()));
}

#[test]
fn the_drawer_shows_the_whole_screen_tail_and_a_row_none() {
    let mut fleet = fleet::canned();
    let tail: Vec<String> = (0..20).map(|n| format!("line {n}")).collect();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 14)
        .unwrap()
        .screen_tail = tail;
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let row = texts_under(&dashboard.tree, row_of(&dashboard, 14));
    assert!(!row.iter().any(|t| t.starts_with("line ")), "{row:?}");

    dashboard.set_detail(Some(14), &mut text);
    let texts = drawer_texts(&dashboard);
    assert!(texts.contains(&"line 0".to_owned()), "{texts:?}");
    assert!(texts.contains(&"line 19".to_owned()));
}

/// dashboard-worker-list AC7: what needs attention outside the filter is
/// counted whether it is a decision or a failure.
#[test]
fn a_visible_failure_still_says_how_many_need_attention_outside() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    key(&mut dashboard, &mut text, Key::Char('/'));
    typed(&mut dashboard, &mut text, "/");
    typed(&mut dashboard, &mut text, "e2e-smoke");
    assert_eq!(rows_of(&dashboard), [19], "only the failure matches");
    let outside = crate::dashboard::list::outside_text(3);
    assert!(
        list_texts(&dashboard).contains(&outside),
        "{:?}",
        list_texts(&dashboard)
    );
}

#[test]
fn a_result_shown_once_is_not_read_again_without_asking() {
    let file = TempFile::new("again.md", b"all green\n");
    let (mut dashboard, mut text) = drawer_on_result(file.0.clone(), true);
    toggle_result(&mut dashboard, &mut text);
    assert!(drawer_texts(&dashboard).contains(&"all green".to_owned()));
    // Away to another worker and back, then closed and opened again.
    dashboard.set_detail(Some(18), &mut text);
    dashboard.set_detail(Some(17), &mut text);
    assert!(!drawer_texts(&dashboard).contains(&"all green".to_owned()));
    toggle_result(&mut dashboard, &mut text);
    dashboard.set_detail(None, &mut text);
    dashboard.set_detail(Some(17), &mut text);
    assert!(!drawer_texts(&dashboard).contains(&"all green".to_owned()));
}

#[test]
fn the_drawer_names_the_direct_parent() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    // fix-tests (14) was spawned by api-refactor (11), under release-0.9.7.
    dashboard.set_detail(Some(14), &mut text);
    let parent = label_of(&dashboard, 11).to_owned();
    let texts = drawer_texts(&dashboard);
    assert!(texts.contains(&format!("ID 14 · 親 {parent}")), "{texts:?}");
}

/// The result path swapped back and forth between a regular file and a
/// FIFO while the drawer reads it: a check of the path before opening it
/// let the open wait for the FIFO's writer (the 262nd read, when this was
/// first run), which froze the dashboard.
#[test]
fn a_result_swapped_for_a_fifo_never_blocks_the_reader() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    let dir = std::env::temp_dir().join(format!("agent-dashboard-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("result.md");
    let regular = dir.join("regular");
    let fifo = dir.join("fifo");
    let swap = dir.join("swap");
    std::fs::write(&regular, b"x").unwrap();
    std::fs::copy(&regular, &path).unwrap();
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    assert!(made.unwrap().success());
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let (stop, path) = (stop.clone(), path.clone());
        let (regular, fifo) = (regular.clone(), fifo.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                for source in [&fifo, &regular] {
                    std::fs::hard_link(source, &swap).unwrap();
                    std::fs::rename(&swap, &path).unwrap();
                }
            }
        })
    };
    let (done, finished) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for _ in 0..5_000 {
            let _ = drawer::read_result(&path);
            let _ = done.send(());
        }
    });
    let blocked = loop {
        match finished.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break false,
            Err(mpsc::RecvTimeoutError::Timeout) => break true,
        }
    };
    stop.store(true, Ordering::Relaxed);
    swapper.join().unwrap();
    if blocked {
        // A writer lets the stuck open return, so the reader can end.
        drop(std::fs::OpenOptions::new().write(true).open(&fifo));
    }
    reader.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!blocked, "read_result waited on a FIFO");
}

#[test]
fn a_result_behind_a_symbolic_link_is_not_read() {
    let file = TempFile::new("target.md", b"secret\n");
    let link = std::env::temp_dir().join(format!("agent-dashboard-{}-link.md", std::process::id()));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&file.0, &link).unwrap();
    let read = drawer::read_result(&link);
    let _ = std::fs::remove_file(&link);
    assert_eq!(read, Err("通常のファイルではありません".to_owned()));
}

#[test]
fn a_rebuild_shows_the_result_as_it_was_read() {
    let file = TempFile::new("kept.md", b"first\n");
    let (mut dashboard, mut text) = drawer_on_result(file.0.clone(), true);
    toggle_result(&mut dashboard, &mut text);
    std::fs::write(&file.0, b"second\n").unwrap();
    // Another worker changes its state: a structural snapshot rebuilds the
    // screen, the drawer with it.
    let mut fleet = dashboard.feed.fleet.clone();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 12)
        .unwrap()
        .state = State::Permission;
    dashboard.apply(live(fleet, 0), &mut text);
    let texts = drawer_texts(&dashboard);
    assert!(texts.contains(&"first".to_owned()), "{texts:?}");
    assert!(!texts.contains(&"second".to_owned()), "{texts:?}");
}

#[test]
fn a_root_agent_has_no_parent_in_the_drawer() {
    let mut fleet = fleet::canned();
    for worker in &mut fleet.workers {
        if worker.pane == 11 {
            worker.orchestrator = worker.label.clone();
            worker.orchestrator_pane = Some(11);
            worker.parent = None;
        }
    }
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    dashboard.set_detail(Some(11), &mut text);
    let texts = drawer_texts(&dashboard);
    assert!(texts.contains(&"ID 11 · 親 なし".to_owned()), "{texts:?}");
}

#[test]
fn a_filter_that_matches_only_a_quiet_worker_shows_it_running() {
    let mut fleet = fleet::canned();
    fleet.workers.retain(|w| !matches!(w.pane, 14..=16));
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    key(&mut dashboard, &mut text, Key::Char('/'));
    typed(&mut dashboard, &mut text, "/");
    typed(&mut dashboard, &mut text, "perf-probe");
    assert_eq!(rows_of(&dashboard), [13]);
    let texts = texts_under(&dashboard.tree, row_of(&dashboard, 13));
    assert_eq!(texts[0], "実行中", "{texts:?}");
    assert_eq!(segment_count(&dashboard, Scope::Attention), 0);
    // e2e-smoke's failure is outside the filter.
    let outside = crate::dashboard::list::outside_text(1);
    assert!(list_texts(&dashboard).contains(&outside));
}

#[test]
fn a_result_gone_and_back_is_not_shown_unasked() {
    let file = TempFile::new("turn-1.md", b"first turn\n");
    let (mut dashboard, mut text) = drawer_on_result(file.0.clone(), true);
    toggle_result(&mut dashboard, &mut text);
    assert!(drawer_texts(&dashboard).contains(&"first turn".to_owned()));
    // The worker runs again (no result), then finishes with a new one.
    let mut fleet = dashboard.feed.fleet.clone();
    let worker = fleet.workers.iter_mut().find(|w| w.pane == 17).unwrap();
    worker.state = State::Running;
    worker.result = None;
    dashboard.apply(live(fleet.clone(), 0), &mut text);
    let next = TempFile::new("turn-2.md", b"second turn\n");
    let worker = fleet.workers.iter_mut().find(|w| w.pane == 17).unwrap();
    worker.state = State::Finished;
    worker.result = Some(next.0.clone());
    dashboard.apply(live(fleet, 0), &mut text);
    let texts = drawer_texts(&dashboard);
    assert!(!texts.contains(&"first turn".to_owned()), "{texts:?}");
    assert!(!texts.contains(&"second turn".to_owned()), "{texts:?}");
}

#[test]
fn a_lone_agent_has_no_parent_in_the_drawer() {
    let mut fleet = fleet::canned();
    for worker in &mut fleet.workers {
        if worker.pane == 11 {
            worker.orchestrator = crate::live::ALONE.to_owned();
            worker.orchestrator_pane = None;
            worker.parent = None;
        }
    }
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    dashboard.set_detail(Some(11), &mut text);
    let texts = drawer_texts(&dashboard);
    assert!(texts.contains(&"ID 11 · 親 なし".to_owned()), "{texts:?}");
}

#[test]
fn a_cut_result_ends_on_a_whole_character() {
    let file = TempFile::new("wide.md", "あ".repeat(6000).as_bytes());
    let (text, cut) = drawer::read_result(&file.0).unwrap();
    assert!(cut);
    assert!(!text.contains('\u{FFFD}'), "a split character");
    assert!(text.len() as u64 > drawer::RESULT_MAX_BYTES - 3);
}
