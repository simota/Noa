// ── Kitty graphics (APC → image store → replies) ────────────────────

/// Build an APC Kitty graphics sequence `ESC _ G <ctrl> ; <base64(data)> ESC \`.
fn kitty_apc(ctrl: &str, data: &[u8]) -> Vec<u8> {
    let mut b64 = Vec::new();
    crate::osc::encode_base64(data, &mut b64);
    let mut out = b"\x1b_G".to_vec();
    out.extend_from_slice(ctrl.as_bytes());
    out.push(b';');
    out.extend_from_slice(&b64);
    out.extend_from_slice(b"\x1b\\");
    out
}

#[test]
fn kitty_transmit_stores_and_replies_ok() {
    let t = run(&kitty_apc("a=t,f=32,s=1,v=1,i=1", &[1, 2, 3, 4]));
    assert!(t.kitty_images.get(1).is_some());
    assert_eq!(t.pending_writes, b"\x1b_Gi=1;OK\x1b\\");
}

#[test]
fn kitty_quiet_one_suppresses_ok() {
    let t = run(&kitty_apc("a=t,f=32,s=1,v=1,i=1,q=1", &[1, 2, 3, 4]));
    assert!(t.kitty_images.get(1).is_some());
    assert!(t.pending_writes.is_empty(), "q=1 suppresses the OK reply");
}

#[test]
fn kitty_quiet_two_suppresses_errors() {
    // Bad dimensions → ENODATA, but q=2 suppresses even errors.
    let t = run(&kitty_apc("a=t,f=32,s=4,v=4,i=1,q=2", &[0; 8]));
    assert!(t.pending_writes.is_empty(), "q=2 suppresses error replies");
}

#[test]
fn kitty_no_reply_when_neither_i_nor_i_number_given() {
    let t = run(&kitty_apc("a=t,f=32,s=1,v=1", &[1, 2, 3, 4]));
    assert!(t.pending_writes.is_empty(), "i=0 and I=0 → no reply at all");
}

#[test]
fn kitty_auto_id_reply_echoes_assigned_id_and_number() {
    let t = run(&kitty_apc("a=t,f=32,s=1,v=1,I=7", &[1, 2, 3, 4]));
    // Auto-assigned id 1, number echoed.
    assert_eq!(t.pending_writes, b"\x1b_Gi=1,I=7;OK\x1b\\");
}

#[test]
fn kitty_error_reply_carries_code() {
    let t = run(&kitty_apc("a=t,f=32,s=4,v=4,i=1", &[0; 8]));
    assert_eq!(
        t.pending_writes,
        b"\x1b_Gi=1;ENODATA:data size mismatch\x1b\\"
    );
}

#[test]
fn kitty_query_validates_without_storing() {
    let t = run(&kitty_apc("a=q,f=32,s=1,v=1,i=9", &[0; 4]));
    assert!(t.kitty_images.get(9).is_none(), "query must not store");
    assert_eq!(t.pending_writes, b"\x1b_Gi=9;OK\x1b\\");
}

#[test]
fn kitty_full_reset_clears_store() {
    let mut t = run(&kitty_apc("a=t,f=32,s=1,v=1,i=1", &[1, 2, 3, 4]));
    assert!(t.kitty_images.get(1).is_some());
    let mut s = Stream::new();
    s.feed(b"\x1bc", &mut t); // RIS
    assert!(
        t.kitty_images.get(1).is_none(),
        "RIS clears the image store"
    );
}

// ── Kitty graphics placements ───────────────────────────────────────

/// A 20×24 terminal with 10×20 px cells (metrics that `a=T`/`a=p` need).
fn kitty_terminal() -> Terminal {
    let mut t = Terminal::new(GridSize::new(20, 24));
    t.set_pixel_metrics(10, 20, 200, 480);
    t
}

fn feed(t: &mut Terminal, bytes: &[u8]) {
    let mut s = Stream::new();
    s.feed(bytes, t);
}

#[test]
fn kitty_transmit_and_display_creates_placement_and_moves_cursor() {
    let mut t = kitty_terminal();
    // 25x40 px image → ceil(25/10)=3 cols, ceil(40/20)=2 rows.
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=25,v=40,i=1", &vec![0u8; 25 * 40 * 4]),
    );
    let placements = &t.primary.kitty_placements;
    assert_eq!(placements.len(), 1);
    assert_eq!((placements[0].cols, placements[0].rows), (3, 2));
    // Cursor: last row of the image (down 1), one column past the right edge (0+3).
    assert_eq!((t.primary.cursor.x, t.primary.cursor.y), (3, 1));
}

#[test]
fn kitty_placement_count_is_hard_capped() {
    let mut t = kitty_terminal();
    let placement = |id: u32| crate::KittyPlacement {
        image_id: 1,
        placement_id: id,
        anchor_abs_row: 0,
        anchor_col: 0,
        cell_x_off: 0,
        cell_y_off: 0,
        src: None,
        cols: 1,
        rows: 1,
        z: 0,
        is_virtual: false,
    };
    let cap = crate::screen::KITTY_PLACEMENT_CAP;
    for id in 0..(cap as u32 + 8) {
        t.primary.insert_kitty_placement(placement(id));
    }
    let placements = &t.primary.kitty_placements;
    assert_eq!(placements.len(), cap);
    // Oldest placements were evicted; the newest survive.
    assert_eq!(placements[0].placement_id, 8);
    assert_eq!(placements.last().unwrap().placement_id, cap as u32 + 7);
}

#[test]
fn kitty_cursor_no_move_keeps_cursor() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    assert_eq!((t.primary.cursor.x, t.primary.cursor.y), (0, 0));
    assert_eq!(t.primary.kitty_placements.len(), 1);
}

#[test]
fn kitty_explicit_columns_rows_override_natural_size() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,c=5,r=4,i=1", &vec![0u8; 10 * 20 * 4]),
    );
    let p = &t.primary.kitty_placements[0];
    assert_eq!((p.cols, p.rows), (5, 4));
}

#[test]
fn kitty_place_without_cell_metrics_is_einval() {
    let mut t = Terminal::new(GridSize::new(20, 24)); // no set_pixel_metrics
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1", &vec![0u8; 10 * 20 * 4]),
    );
    assert!(t.primary.kitty_placements.is_empty());
    assert_eq!(t.pending_writes, b"\x1b_Gi=1;EINVAL:invalid request\x1b\\");
}

#[test]
fn kitty_put_displays_transmitted_image() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=t,f=32,s=10,v=20,i=5", &vec![0u8; 10 * 20 * 4]),
    );
    assert!(
        t.primary.kitty_placements.is_empty(),
        "a=t alone doesn't place"
    );
    feed(&mut t, b"\x1b_Ga=p,i=5\x1b\\");
    assert_eq!(t.primary.kitty_placements.len(), 1);
    assert_eq!(t.primary.kitty_placements[0].image_id, 5);
}

#[test]
fn kitty_put_missing_image_is_enoent() {
    let mut t = kitty_terminal();
    feed(&mut t, b"\x1b_Ga=p,i=99\x1b\\");
    assert_eq!(t.pending_writes, b"\x1b_Gi=99;ENOENT:file not found\x1b\\");
}

#[test]
fn kitty_unnamed_placement_overwrites() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=t,f=32,s=10,v=20,i=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b_Ga=p,i=1\x1b\\");
    feed(&mut t, b"\x1b_Ga=p,i=1\x1b\\");
    assert_eq!(
        t.primary.kitty_placements.len(),
        1,
        "second unnamed placement overwrites the first"
    );
}

#[test]
fn kitty_delete_all_placements() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=2", &vec![0u8; 10 * 20 * 4]),
    );
    assert_eq!(t.primary.kitty_placements.len(), 2);
    feed(&mut t, b"\x1b_Ga=d,d=a\x1b\\");
    assert!(t.primary.kitty_placements.is_empty());
    // Lowercase d=a keeps image data.
    assert!(t.kitty_images.get(1).is_some());
    assert!(t.kitty_images.get(2).is_some());
}

#[test]
fn kitty_delete_by_id_uppercase_frees_data() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b_Ga=d,d=I,i=1\x1b\\");
    assert!(t.primary.kitty_placements.is_empty());
    assert!(
        t.kitty_images.get(1).is_none(),
        "uppercase d frees the image"
    );
}

#[test]
fn kitty_delete_at_cursor() {
    let mut t = kitty_terminal();
    // Place at (0,0) spanning 1x1, then move cursor onto it and delete d=c.
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b[1;1H"); // cursor home, over the placement
    feed(&mut t, b"\x1b_Ga=d,d=c\x1b\\");
    assert!(t.primary.kitty_placements.is_empty());
}

#[test]
fn kitty_delete_by_z() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,z=5,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=2,z=9,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b_Ga=d,d=z,z=5\x1b\\");
    assert_eq!(t.primary.kitty_placements.len(), 1);
    assert_eq!(t.primary.kitty_placements[0].image_id, 2);
}

#[test]
fn kitty_ed2_removes_intersecting_placements() {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b[2J"); // ED 2
    assert!(t.primary.kitty_placements.is_empty());
}

#[test]
fn kitty_visible_placement_projects_into_viewport() {
    let mut t = kitty_terminal();
    // Place at row 5.
    feed(&mut t, b"\x1b[6;1H");
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=40,i=1,C=1", &vec![0u8; 10 * 40 * 4]),
    );
    let vis = t.kitty_visible_placements();
    assert_eq!(vis.len(), 1);
    assert_eq!(vis[0].grid_y, 5);
    assert_eq!((vis[0].cols, vis[0].rows), (1, 2));
    assert!(t.kitty_image(1).is_some());
}

#[test]
fn kitty_scroll_pushes_placement_up_via_absolute_anchor() {
    let mut t = kitty_terminal();
    feed(&mut t, b"\x1b[6;1H");
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    assert_eq!(t.kitty_visible_placements()[0].grid_y, 5);
    // Scroll the whole screen up by 3 lines' worth of newlines from the bottom.
    feed(&mut t, b"\x1b[24;1H"); // last row
    feed(&mut t, b"\n\n\n");
    let vis = t.kitty_visible_placements();
    assert_eq!(vis.len(), 1, "placement follows content into scrollback");
    assert_eq!(vis[0].grid_y, 2, "moved up by 3 rows");
}

#[test]
fn kitty_alt_screen_placement_is_separated() {
    let mut t = kitty_terminal();
    feed(&mut t, b"\x1b[?1049h"); // enter alt screen
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    assert_eq!(t.active().kitty_placements.len(), 1);
    feed(&mut t, b"\x1b[?1049l"); // leave alt screen
    assert!(
        t.active().kitty_placements.is_empty(),
        "alt-screen placement vanishes on return to primary"
    );
    // Image data survives (only placements are per-screen).
    assert!(t.kitty_images.get(1).is_some());
}

#[test]
fn kitty_reflow_reanchors_placement_to_same_content_row() {
    let mut t = Terminal::new(GridSize::new(20, 4));
    t.set_pixel_metrics(10, 20, 200, 80);
    // A 30-char logical line at the top wraps into two rows at 20 cols.
    feed(&mut t, b"\x1b[H");
    feed(&mut t, &[b'A'; 30]);
    feed(&mut t, b"\r\nIMGROW\r");
    // Place a 1×1 image over the IMGROW row (C=1 keeps the cursor put).
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    // Scroll once from the bottom so the wrapped line moves into scrollback.
    feed(&mut t, b"\x1b[4;1H\r\nTAIL\n");

    let find_imgrow = |t: &Terminal| -> i32 {
        (0..t.primary.rows as usize)
            .find(|&y| row_text(t, y, 6) == "IMGROW")
            .map(|y| y as i32)
            .expect("IMGROW still on screen")
    };
    let before = t.kitty_visible_placements();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].grid_y, find_imgrow(&t), "anchored on IMGROW row");

    // Widen so the 30-char line un-wraps to one row: the scrollback shrinks by a
    // row and every row below shifts up. The placement must track IMGROW.
    t.resize(GridSize::new(40, 4));

    let after = t.kitty_visible_placements();
    assert_eq!(after.len(), 1, "placement survives the reflow");
    assert_eq!(
        after[0].grid_y,
        find_imgrow(&t),
        "placement follows IMGROW to its new row"
    );
}

#[test]
fn kitty_reflow_drops_placement_whose_anchor_is_discarded() {
    let mut t = Terminal::new(GridSize::new(20, 4));
    t.set_pixel_metrics(10, 20, 200, 80);
    // Six short lines: L0/L1 spill into scrollback, L2..L5 fill the grid.
    for i in 0..6 {
        feed(&mut t, format!("L{i}\r\n").as_bytes());
    }
    // Place a 1×1 image on the last content row, then move the cursor to the top.
    feed(&mut t, b"\x1b[4;1H");
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    feed(&mut t, b"\x1b[1;1H");
    assert_eq!(t.primary.kitty_placements.len(), 1);

    // Reflow with the cursor near the top drops the rows below the grid window,
    // including the placement's anchor line — the content is gone, so is it.
    t.resize(GridSize::new(40, 4));
    assert!(
        t.primary.kitty_placements.is_empty(),
        "a placement whose anchor content the reflow discards is removed"
    );
}

#[test]
fn kitty_placement_pruned_when_its_row_is_evicted() {
    let mut t = Terminal::new(GridSize::new(20, 4));
    t.set_pixel_metrics(10, 20, 200, 80);
    t.set_scrollback_limit_bytes(1); // keep essentially no history
                                     // Place a 1×1 image at the top, then scroll far past it. Eviction is
                                     // page-granular, so it takes more than a page of full-width rows to strand
                                     // the anchor.
    feed(&mut t, b"\x1b[1;1H");
    feed(
        &mut t,
        &kitty_apc("a=T,f=32,s=10,v=20,i=1,C=1", &vec![0u8; 10 * 20 * 4]),
    );
    assert_eq!(t.primary.kitty_placements.len(), 1);
    let mut s = Stream::new();
    feed_full_rows(&mut s, &mut t, 20, 1000);

    assert!(t.primary.rows_evicted() > 1, "the anchor row scrolled off");
    assert!(
        t.primary.kitty_placements.is_empty(),
        "eviction prunes the stranded placement"
    );
    // The image data lingers but nothing references it now, so a quota sweep is
    // free to reclaim it (it no longer appears in the referenced-id set).
    assert!(t.kitty_images.get(1).is_some());
}

// ── Kitty Unicode placeholders (U+10EEEE) ───────────────────────────

/// Row/column/most-significant-byte diacritics for values 0, 1, 2 (the first
/// three entries of Kitty's table).
const DIA: [char; 3] = ['\u{0305}', '\u{030D}', '\u{030E}'];

/// Write a placeholder cell (`U+10EEEE`) at grid `(x, y)`, encoding image id in
/// the fg, placement id in the underline color, and the given diacritics.
fn put_placeholder(t: &mut Terminal, x: usize, y: usize, id: u32, diacritics: &[char]) {
    let cell = &mut t.primary.grid[y].cells[x];
    cell.ch = crate::PLACEHOLDER;
    cell.fg = Color::Rgb(Rgb::new(
        ((id >> 16) & 0xff) as u8,
        ((id >> 8) & 0xff) as u8,
        (id & 0xff) as u8,
    ));
    cell.combining.clear();
    for &d in diacritics {
        cell.combining.push(d);
    }
}

/// A 20×24 terminal holding image id 1 (30×40 px) placed as a virtual 3×2 cell
/// grid, so each virtual cell maps to a clean 10×20 px image tile.
fn kitty_virtual_terminal() -> Terminal {
    let mut t = kitty_terminal();
    feed(
        &mut t,
        &kitty_apc(
            "a=T,f=32,s=30,v=40,i=1,U=1,c=3,r=2,C=1",
            &vec![0u8; 30 * 40 * 4],
        ),
    );
    // The virtual placement is stored but excluded from direct rendering.
    assert_eq!(t.primary.kitty_placements.len(), 1);
    assert!(t.primary.kitty_placements[0].is_virtual);
    assert!(t.kitty_visible_placements().is_empty());
    t
}

#[test]
fn placeholder_run_resolves_source_tile() {
    let mut t = kitty_virtual_terminal();
    // Row 0 of the image across all three columns: first cell fully specified,
    // the next two infer column +1.
    put_placeholder(&mut t, 0, 0, 1, &[DIA[0], DIA[0]]);
    put_placeholder(&mut t, 1, 0, 1, &[]);
    put_placeholder(&mut t, 2, 0, 1, &[]);

    let placements = t.kitty_placeholder_placements();
    assert_eq!(placements.len(), 1, "three cells fuse into one run");
    let p = &placements[0];
    assert_eq!((p.grid_x, p.grid_y), (0, 0));
    assert_eq!((p.cols, p.rows), (3, 1));
    assert_eq!(p.image_id, 1);
    // Whole first image row: x=0, y=0, w=30 (3×10), h=20 (40/2).
    assert_eq!(p.src, Some([0, 0, 30, 20]));
    assert_eq!(p.z, 0);
}

#[test]
fn placeholder_second_row_offsets_source_y() {
    let mut t = kitty_virtual_terminal();
    // Image row 1, single column 0 → lower tile of the image.
    put_placeholder(&mut t, 4, 2, 1, &[DIA[1], DIA[0]]);
    let placements = t.kitty_placeholder_placements();
    assert_eq!(placements.len(), 1);
    let p = &placements[0];
    assert_eq!((p.grid_x, p.grid_y), (4, 2));
    assert_eq!((p.cols, p.rows), (1, 1));
    assert_eq!(p.src, Some([0, 20, 10, 20]), "image row 1 starts at y=20");
}

#[test]
fn placeholder_without_virtual_placement_draws_nothing() {
    let mut t = kitty_terminal();
    // A placeholder referencing image 7, which has no virtual placement.
    put_placeholder(&mut t, 0, 0, 7, &[DIA[0], DIA[0]]);
    assert!(
        t.kitty_placeholder_placements().is_empty(),
        "no virtual placement ⇒ nothing to resolve"
    );
}

#[test]
fn placeholder_id_mismatch_is_skipped() {
    let mut t = kitty_virtual_terminal();
    // Virtual placement is for image 1; a placeholder for image 2 resolves to
    // nothing even though a virtual placement exists.
    put_placeholder(&mut t, 0, 0, 2, &[DIA[0], DIA[0]]);
    assert!(t.kitty_placeholder_placements().is_empty());
}

#[test]
fn placeholder_column_jump_splits_run() {
    let mut t = kitty_virtual_terminal();
    // Column 0 then an explicit jump to column 2 (skipping 1) ⇒ two runs.
    put_placeholder(&mut t, 0, 0, 1, &[DIA[0], DIA[0]]);
    put_placeholder(&mut t, 1, 0, 1, &[DIA[0], DIA[2]]);
    let placements = t.kitty_placeholder_placements();
    assert_eq!(
        placements.len(),
        2,
        "non-contiguous image columns don't fuse"
    );
    assert_eq!(placements[0].src, Some([0, 0, 10, 20]));
    assert_eq!(placements[1].src, Some([20, 0, 10, 20]), "column 2 tile");
}
