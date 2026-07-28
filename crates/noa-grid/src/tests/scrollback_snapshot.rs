// End-to-end `Terminal` ↔ snapshot round-trips: capture on one terminal,
// restore into a fresh one, the way `scrollback-persist` does across a quit.

use crate::snapshot;

/// Capture `source` and restore it into a brand-new `cols`×`rows` terminal,
/// the way a relaunch does.
fn restore_into(source: &Terminal, cols: u16, rows: u16, max_bytes: usize) -> Terminal {
    let bytes = source
        .scrollback_snapshot_bytes(max_bytes, 1_700_000_000, None)
        .expect("a terminal with output produces a snapshot");
    let decoded = snapshot::decode(&bytes).expect("a snapshot noa just wrote decodes");
    let mut restored = Terminal::new(GridSize::new(cols, rows));
    restored.restore_scrollback_snapshot(decoded);
    restored
}

#[test]
fn a_captured_session_reads_back_as_history_in_a_fresh_terminal() {
    let source = run_size(20, 4, b"first\r\nsecond\r\nthird\r\nfourth\r\nfifth");
    let mut restored = restore_into(&source, 20, 4, 1 << 20);

    let text = restored.scrollback_text().expect("restored history is text");
    for line in ["first", "second", "third", "fourth", "fifth"] {
        assert!(text.contains(line), "{line:?} missing from {text:?}");
    }
}

#[test]
fn restored_history_keeps_its_colors() {
    // Red "boom", then a default-pen line.
    let source = run_size(20, 4, b"\x1b[31mboom\x1b[0m\r\nplain\r\n");
    let restored = restore_into(&source, 20, 4, 1 << 20);

    let row = restored
        .active_absolute_row(0)
        .expect("the first restored row is addressable");
    assert_eq!(row.cells[0].ch, 'b');
    assert_eq!(
        row.cells[0].fg,
        Color::Palette(1),
        "an error line restored gray would lose the only cue a beginner has"
    );
}

#[test]
fn restored_history_keeps_bold_and_underline_attributes() {
    let source = run_size(20, 3, b"\x1b[1;4mloud\x1b[0m\r\n");
    let restored = restore_into(&source, 20, 3, 1 << 20);

    let row = restored.active_absolute_row(0).expect("row 0 exists");
    assert!(row.cells[0].attrs.contains(CellAttrs::BOLD));
    assert!(row.cells[0].attrs.contains(CellAttrs::UNDERLINE));
}

#[test]
fn restored_history_keeps_hyperlinks_resolvable_in_the_new_registry() {
    let source = run_size(20, 3, b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\\r\n");
    let restored = restore_into(&source, 20, 3, 1 << 20);

    let row = restored.active_absolute_row(0).expect("row 0 exists");
    let id = row.cells[0]
        .hyperlink
        .expect("the linked cell keeps a link id");
    assert_eq!(
        restored.hyperlinks[id.get()].uri, "https://example.com",
        "the id must resolve in the *restoring* terminal's registry"
    );
}

#[test]
fn a_snapshot_restores_into_a_wider_terminal_without_frozen_wraps() {
    // 6 columns: "abcdefgh" soft-wraps onto a second row.
    let source = run_size(6, 3, b"abcdefgh\r\n");
    let mut restored = restore_into(&source, 12, 3, 1 << 20);

    let text = restored.scrollback_text().expect("restored history is text");
    assert!(
        text.contains("abcdefgh"),
        "the logical line must rejoin at the new width: {text:?}"
    );
    let row = restored.active_absolute_row(0).expect("row 0 exists");
    assert_eq!(row.cells.len(), 12, "rows are re-laid at the new width");
    assert!(!row.wrapped, "the line now fits on one row");
}

#[test]
fn a_snapshot_restores_into_a_narrower_terminal_by_rewrapping() {
    let source = run_size(12, 3, b"abcdefgh\r\n");
    let mut restored = restore_into(&source, 4, 3, 1 << 20);

    let text = restored.scrollback_text().expect("restored history is text");
    assert!(text.contains("abcdefgh"), "{text:?}");
    let first = restored.active_absolute_row(0).expect("row 0 exists");
    assert_eq!(first.cells.len(), 4);
    assert!(first.wrapped, "the split rows stay one logical line");
}

#[test]
fn the_alternate_screen_is_never_captured() {
    // Write shell output, enter the alternate screen, fill it, and capture
    // from there: what a pane restores must be the shell history underneath,
    // not a dead frame of a full-screen app.
    let source = run_size(20, 4, b"shell output\r\n\x1b[?1049hFULLSCREEN APP\r\n");
    assert!(source.active_is_alt, "the fixture must be on the alt screen");

    let mut restored = restore_into(&source, 20, 4, 1 << 20);
    let text = restored.scrollback_text().expect("restored history is text");
    assert!(text.contains("shell output"), "{text:?}");
    assert!(
        !text.contains("FULLSCREEN APP"),
        "alt-screen contents leaked into the record: {text:?}"
    );
}

#[test]
fn a_terminal_that_produced_nothing_has_no_snapshot() {
    let source = run_size(20, 4, b"");
    assert!(source.scrollback_snapshot_bytes(1 << 20, 0, None).is_none());
}

#[test]
fn a_zero_budget_captures_nothing() {
    let source = run_size(20, 4, b"something\r\n");
    assert!(source.scrollback_snapshot_bytes(0, 0, None).is_none());
}

#[test]
fn a_small_budget_keeps_the_newest_lines() {
    let source = run_size(20, 4, b"oldest\r\nmiddle\r\nnewest\r\n");
    // Room for roughly one row.
    let mut restored = restore_into(&source, 20, 4, 80);

    let text = restored.scrollback_text().expect("restored history is text");
    assert!(text.contains("newest"), "the newest line must survive: {text:?}");
    assert!(
        !text.contains("oldest"),
        "the budget must drop the oldest line: {text:?}"
    );
}

#[test]
fn restoring_leaves_the_live_grid_and_cursor_alone() {
    let source = run_size(20, 4, b"history\r\n");
    let bytes = source
        .scrollback_snapshot_bytes(1 << 20, 0, None)
        .expect("snapshot");
    let decoded = snapshot::decode(&bytes).expect("decodes");

    let mut restored = Terminal::new(GridSize::new(20, 4));
    let cursor_before = restored.primary.cursor.y;
    let inserted = restored.restore_scrollback_snapshot(decoded);

    assert!(inserted > 0);
    assert_eq!(
        restored.primary.cursor.y, cursor_before,
        "restored rows are history, not replayed input"
    );
    assert_eq!(restored.title, "", "a snapshot cannot set the window title");
    assert!(
        restored.take_pending_writes().is_empty(),
        "a snapshot cannot make the terminal write to the pty"
    );
}

#[test]
fn a_corrupt_snapshot_file_leaves_the_terminal_untouched() {
    let mut restored = Terminal::new(GridSize::new(20, 4));
    assert!(snapshot::decode(b"\x00garbage\xff").is_none());
    assert_eq!(restored.scrollback_text(), None, "nothing was inserted");
}

#[test]
fn restoring_twice_stacks_the_older_record_first() {
    let older = run_size(20, 3, b"older\r\n");
    let newer = run_size(20, 3, b"newer\r\n");

    let mut restored = Terminal::new(GridSize::new(20, 3));
    // Newest first, then older: each prepend goes ahead of what is there, so
    // the reading order ends up chronological.
    for source in [&newer, &older] {
        let bytes = source
            .scrollback_snapshot_bytes(1 << 20, 0, None)
            .expect("snapshot");
        restored.restore_scrollback_snapshot(snapshot::decode(&bytes).expect("decodes"));
    }

    let text = restored.scrollback_text().expect("restored history is text");
    let older_at = text.find("older").expect("older present");
    let newer_at = text.find("newer").expect("newer present");
    assert!(older_at < newer_at, "chronological order: {text:?}");
}

#[test]
fn a_skipped_row_is_left_out_of_the_capture() {
    // The app inserts a synthetic separator row when it restores a record.
    // Capturing it would bake it into the next record, and every relaunch
    // would leave one more behind until the history is a stack of separators.
    let source = run_size(20, 4, b"real output\r\nSEPARATOR\r\nmore output\r\n");
    let separator_abs = source
        .active_absolute_row(1)
        .map(|_| 1)
        .expect("row 1 exists");

    let bytes = source
        .scrollback_snapshot_bytes(1 << 20, 0, Some(separator_abs))
        .expect("snapshot");
    let decoded = snapshot::decode(&bytes).expect("decodes");
    let text: Vec<String> = decoded
        .rows
        .iter()
        .map(|row| row.cells.iter().map(|c| c.ch).collect::<String>().trim_end().to_string())
        .collect();

    assert!(text.iter().any(|line| line == "real output"), "{text:?}");
    assert!(text.iter().any(|line| line == "more output"), "{text:?}");
    assert!(
        !text.iter().any(|line| line == "SEPARATOR"),
        "the skipped row must not survive: {text:?}"
    );
}

fn snapshot_text(source: &Terminal, skip: Option<usize>) -> Vec<String> {
    let bytes = source
        .scrollback_snapshot_bytes(1 << 20, 0, skip)
        .expect("snapshot");
    snapshot::decode(&bytes)
        .expect("decodes")
        .rows
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn a_skipped_row_above_the_retained_range_is_ignored() {
    let source = run_size(20, 4, b"alpha\r\nbeta\r\n");
    let text = snapshot_text(&source, Some(usize::MAX));
    assert!(text.iter().any(|line| line == "alpha"), "{text:?}");
    assert!(text.iter().any(|line| line == "beta"), "{text:?}");
}

#[test]
fn a_skipped_row_below_the_eviction_point_is_ignored() {
    // The real stale-index case: an index from before eviction. `usize::MAX`
    // exercises the "too large" branch instead, which is a different path —
    // this one has to actually evict first.
    // Eviction is page-granular and a page is 64 KiB, so the fixture needs
    // enough rows to fill more than one page before anything is dropped.
    let mut source = Terminal::new(GridSize::new(80, 3));
    source.primary.set_scrollback_limit_bytes(64 * 1024);
    let mut stream = noa_vt::Stream::new();
    for line in 0..3000 {
        stream.feed(format!("line{line}\r\n").as_bytes(), &mut source);
    }
    let evicted = source.active_oldest_row();
    assert!(evicted > 0, "the fixture must have evicted rows");

    let text = snapshot_text(&source, Some(evicted - 1));

    let unskipped = snapshot_text(&source, None);
    assert_eq!(
        text, unskipped,
        "an index below the retained range must skip nothing at all"
    );
    assert!(
        text.iter().any(|line| line == "line2999"),
        "the newest line must survive"
    );
}

#[test]
fn a_skipped_row_landing_on_a_live_row_drops_exactly_that_row() {
    // The dangerous direction: a *valid-looking* index that names live output.
    // Nothing stops the caller passing one, which is why the app guards it with
    // the terminal's coordinate generation.
    let source = run_size(20, 5, b"keep-one\r\nvictim\r\nkeep-two\r\n");
    let text = snapshot_text(&source, Some(1));
    assert!(text.iter().any(|line| line == "keep-one"), "{text:?}");
    assert!(text.iter().any(|line| line == "keep-two"), "{text:?}");
    assert!(
        !text.iter().any(|line| line == "victim"),
        "skip_row is unconditional by design: {text:?}"
    );
}
