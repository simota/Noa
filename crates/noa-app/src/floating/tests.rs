//! Model-level acceptance tests (`docs/specs/floating-panes.acceptance.md`).
//! Fixture F-1: split area 1000×600 px at the origin, cell 10×20 px, title
//! strip 20 px; panes A (x 0..500) and B (x 500..1000) when a case names them.

use super::*;

const M: FloatMetrics = FloatMetrics {
    cell_w: 10,
    cell_h: 20,
    pad_w: 0,
    pad_h: 0,
    pad_left: 0,
};
const AREA: Rect = Rect::new(0, 0, 1000, 600);
const A: PaneId = PaneId::new(1);
const B: PaneId = PaneId::new(2);
const SINGLE: [(PaneId, Rect); 1] = [(A, AREA)];
const SPLIT: [(PaneId, Rect); 2] = [
    (A, Rect::new(0, 0, 500, 600)),
    (B, Rect::new(500, 0, 500, 600)),
];

fn layout(panes: &[(PaneId, Rect)]) -> FloatLayout<'_> {
    FloatLayout {
        tab: AREA,
        window: AREA,
        panes,
        zoomed: None,
    }
}

fn id(raw: u64) -> PaneId {
    PaneId::new(100 + raw)
}

/// A float on `anchor` with the given normal-state rect.
fn float_at(
    set: &mut FloatSet,
    raw: u64,
    pane: PaneId,
    panes: &[(PaneId, Rect)],
    rect: Rect,
) -> PaneId {
    let float = id(raw);
    set.create(float, pane, &layout(panes), M).unwrap();
    let anchor = layout(panes).anchor_rect(FloatAnchor::Pane(pane)).unwrap();
    set_from_rect(set.get_mut(float).unwrap(), rect, anchor);
    float
}

fn outer(set: &FloatSet, float: PaneId, panes: &[(PaneId, Rect)]) -> Rect {
    set.placed(float, &layout(panes), M).unwrap().outer
}

fn drag(
    set: &mut FloatSet,
    float: PaneId,
    kind: FloatDragKind,
    from: Point,
    by: (i64, i64),
    panes: &[(PaneId, Rect)],
) -> bool {
    let Some(drag) = set.begin_drag(float, kind, from, &layout(panes), M) else {
        return false;
    };
    let to = Point::new(
        (i64::from(from.x) + by.0).max(0) as u32,
        (i64::from(from.y) + by.1).max(0) as u32,
    );
    set.drag_to(&drag, to, &layout(panes), M);
    true
}

const fn edges(left: bool, right: bool, top: bool, bottom: bool) -> Edges {
    Edges {
        left,
        right,
        top,
        bottom,
    }
}

#[test]
fn tc_001_new_float_takes_the_default_rect_and_grid() {
    let mut set = FloatSet::default();
    set.create(id(1), A, &layout(&SINGLE), M).unwrap();
    let placed = set.placed(id(1), &layout(&SINGLE), M).unwrap();
    assert_eq!(placed.outer, Rect::new(200, 100, 800, 500));
    let content = placed.content.unwrap();
    assert_eq!((content.w / M.cell_w, content.h / M.cell_h), (80, 24));
    assert_eq!(set.get(id(1)).unwrap().anchor, FloatAnchor::Pane(A));
}

#[test]
fn tc_002_later_floats_step_left_and_up_by_the_strip_height() {
    let mut set = FloatSet::default();
    for raw in 1..=3 {
        set.create(id(raw), A, &layout(&SINGLE), M).unwrap();
    }
    assert_eq!(outer(&set, id(2), &SINGLE), Rect::new(180, 80, 800, 500));
    assert_eq!(outer(&set, id(3), &SINGLE), Rect::new(160, 60, 800, 500));
}

#[test]
fn tc_002_new_float_steps_clear_of_visible_floats_on_other_anchors() {
    // A tab-anchored float sits where a new pane float would land, one layer
    // above it: the new float must not open hidden behind it.
    let mut set = FloatSet::default();
    set.create(id(1), A, &layout(&SINGLE), M).unwrap();
    set.cycle_anchor(id(1), &layout(&SINGLE), M).unwrap();
    set.create(id(2), A, &layout(&SINGLE), M).unwrap();
    assert_eq!(outer(&set, id(1), &SINGLE), Rect::new(200, 100, 800, 500));
    assert_eq!(outer(&set, id(2), &SINGLE), Rect::new(180, 80, 800, 500));

    // A float over pane B does not shift a new float on pane A.
    let mut set = FloatSet::default();
    float_at(&mut set, 1, B, &SPLIT, Rect::new(600, 100, 300, 200));
    set.create(id(2), A, &layout(&SPLIT), M).unwrap();
    assert_eq!(outer(&set, id(2), &SPLIT), Rect::new(50, 100, 450, 500));
}

#[test]
fn tc_003_small_anchor_caps_the_float_at_ninety_percent() {
    let panes = [(A, Rect::new(0, 0, 600, 300))];
    let mut set = FloatSet::default();
    set.create(id(1), A, &layout(&panes), M).unwrap();
    let placed = set.placed(id(1), &layout(&panes), M).unwrap();
    assert_eq!((placed.outer.w, placed.outer.h), (540, 270));
    let content = placed.content.unwrap();
    assert_eq!((content.w / M.cell_w, content.h / M.cell_h), (54, 12));
}

#[test]
fn tc_005_move_clamps_on_all_four_sides() {
    for (by, expected) in [((900, 900), (600, 300)), ((-900, -900), (0, 0))] {
        let mut set = FloatSet::default();
        let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(100, 100, 400, 300));
        assert!(drag(
            &mut set,
            float,
            FloatDragKind::Move,
            Point::new(150, 110),
            by,
            &SINGLE
        ));
        let rect = outer(&set, float, &SINGLE);
        assert_eq!(
            (rect.x, rect.y, rect.w, rect.h),
            (expected.0, expected.1, 400, 300)
        );
    }
}

#[test]
fn tc_006_tab_switch_carries_only_window_floats() {
    let mut tab1 = FloatSet::default();
    let pane_float = float_at(&mut tab1, 1, A, &SINGLE, Rect::new(100, 100, 300, 200));
    let tab_float = float_at(&mut tab1, 2, A, &SINGLE, Rect::new(200, 200, 300, 200));
    let window_float = float_at(&mut tab1, 3, A, &SINGLE, Rect::new(600, 300, 300, 200));
    tab1.cycle_anchor(tab_float, &layout(&SINGLE), M).unwrap();
    tab1.cycle_anchor(window_float, &layout(&SINGLE), M)
        .unwrap();
    tab1.cycle_anchor(window_float, &layout(&SINGLE), M)
        .unwrap();
    let before = outer(&tab1, window_float, &SINGLE);

    let tab2_panes = [(PaneId::new(9), AREA)];
    let mut tab2 = FloatSet::default();
    tab2.adopt(tab1.take_anchor(FloatAnchor::Window));

    let shown: Vec<PaneId> = tab2
        .draw_order(&layout(&tab2_panes), M)
        .into_iter()
        .map(|placed| placed.id)
        .collect();
    assert_eq!(shown, vec![window_float]);
    assert_eq!(outer(&tab2, window_float, &tab2_panes), before);
    assert!(tab1.contains(pane_float) && tab1.contains(tab_float));
}

#[test]
fn tc_007_pane_float_hides_while_another_pane_is_zoomed() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SPLIT, Rect::new(50, 100, 300, 200));
    let zoomed = FloatLayout {
        zoomed: Some(B),
        ..layout(&SPLIT)
    };
    assert!(!set.is_visible(float, &zoomed, M));
    assert_eq!(outer(&set, float, &SPLIT), Rect::new(50, 100, 300, 200));
}

#[test]
fn tc_008_cycle_anchor_keeps_the_screen_rect_and_picks_the_pane_under_the_centre() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SPLIT, Rect::new(100, 100, 300, 200));
    let other = float_at(&mut set, 2, A, &SPLIT, Rect::new(10, 10, 300, 200));
    set.cycle_anchor(other, &layout(&SPLIT), M).unwrap();

    assert_eq!(
        set.cycle_anchor(float, &layout(&SPLIT), M),
        Ok(Some(FloatAnchor::Tab))
    );
    assert_eq!(outer(&set, float, &SPLIT), Rect::new(100, 100, 300, 200));
    assert_eq!(set.draw_order(&layout(&SPLIT), M).last().unwrap().id, float);

    assert!(drag(
        &mut set,
        float,
        FloatDragKind::Move,
        Point::new(150, 110),
        (500, 0),
        &SPLIT
    ));
    assert_eq!(outer(&set, float, &SPLIT), Rect::new(600, 100, 300, 200));

    assert_eq!(
        set.cycle_anchor(float, &layout(&SPLIT), M),
        Ok(Some(FloatAnchor::Window))
    );
    assert_eq!(outer(&set, float, &SPLIT), Rect::new(600, 100, 300, 200));

    assert_eq!(
        set.cycle_anchor(float, &layout(&SPLIT), M),
        Ok(Some(FloatAnchor::Pane(B)))
    );
    assert_eq!(outer(&set, float, &SPLIT), Rect::new(600, 100, 300, 200));
}

#[test]
fn tc_009_anchor_resize_applies_ratio_then_minimum_without_rewriting_the_ratio() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(100, 60, 200, 120));
    let held = set
        .begin_drag(
            float,
            FloatDragKind::Move,
            Point::new(150, 70),
            &layout(&SINGLE),
            M,
        )
        .unwrap();

    let small = [(A, Rect::new(0, 0, 500, 300))];
    let small_layout = FloatLayout {
        tab: small[0].1,
        window: small[0].1,
        panes: &small,
        zoomed: None,
    };
    assert_eq!(
        set.placed(float, &small_layout, M).unwrap().outer,
        Rect::new(50, 30, 200, 80)
    );
    assert_eq!(outer(&set, float, &SINGLE), Rect::new(100, 60, 200, 120));
    // The drag survives the anchor change and still moves the float.
    set.drag_to(&held, Point::new(160, 70), &layout(&SINGLE), M);
    assert_eq!(outer(&set, float, &SINGLE).x, 110);
}

#[test]
fn tc_010_float_hides_below_the_minimum_and_returns() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(200, 100, 400, 300));
    let tiny = [(A, Rect::new(0, 0, 150, 60))];
    assert!(!set.is_visible(float, &layout(&tiny), M));
    assert!(set.contains(float));
    assert_eq!(outer(&set, float, &SINGLE), Rect::new(200, 100, 400, 300));
}

#[test]
fn tc_011_layers_are_fixed_and_focus_raises_within_a_layer() {
    let mut set = FloatSet::default();
    let p1 = float_at(&mut set, 1, A, &SINGLE, Rect::new(100, 100, 300, 200));
    let p2 = float_at(&mut set, 2, A, &SINGLE, Rect::new(150, 150, 300, 200));
    let t1 = float_at(&mut set, 3, A, &SINGLE, Rect::new(200, 200, 300, 200));
    let w1 = float_at(&mut set, 4, A, &SINGLE, Rect::new(250, 250, 300, 200));
    set.cycle_anchor(t1, &layout(&SINGLE), M).unwrap();
    set.cycle_anchor(w1, &layout(&SINGLE), M).unwrap();
    set.cycle_anchor(w1, &layout(&SINGLE), M).unwrap();
    let front_to_back = |set: &FloatSet| -> Vec<PaneId> {
        set.draw_order(&layout(&SINGLE), M)
            .into_iter()
            .rev()
            .map(|placed| placed.id)
            .collect()
    };

    set.note_focus(p1);
    assert_eq!(front_to_back(&set), vec![w1, t1, p1, p2]);

    let next = set.cycle_from(Some(p1), true, &layout(&SINGLE), M).unwrap();
    assert_eq!(next, p2);
    set.note_focus(next);
    assert_eq!(front_to_back(&set), vec![w1, t1, p2, p1]);
}

#[test]
fn tc_012_collapse_keeps_width_and_corner_and_never_changes_the_grid() {
    let mut set = FloatSet::default();
    set.create(id(1), A, &layout(&SINGLE), M).unwrap();
    let before = set.placed(id(1), &layout(&SINGLE), M).unwrap();

    assert_eq!(set.toggle_collapse(id(1)), Some(FloatState::Collapsed));
    let collapsed = set.placed(id(1), &layout(&SINGLE), M).unwrap();
    assert_eq!(
        collapsed.outer,
        Rect::new(before.outer.x, before.outer.y, before.outer.w, 20)
    );
    // No content rect while collapsed: the app has nothing to resize a pty to.
    assert_eq!(collapsed.content, None);
    assert!(set.cycle_targets(&layout(&SINGLE), M).is_empty());

    assert_eq!(set.toggle_collapse(id(1)), Some(FloatState::Normal));
    assert_eq!(set.placed(id(1), &layout(&SINGLE), M).unwrap(), before);
}

#[test]
fn tc_013_maximize_fills_the_anchor_and_restores() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(200, 100, 400, 300));
    assert_eq!(set.toggle_maximize(float), Some(FloatState::Maximized));
    let placed = set.placed(float, &layout(&SINGLE), M).unwrap();
    assert_eq!(placed.outer, AREA);
    assert_eq!(placed.strip, Rect::new(0, 0, 1000, 20));
    let content = placed.content.unwrap();
    assert_eq!((content.w / M.cell_w, content.h / M.cell_h), (100, 29));

    assert_eq!(set.toggle_maximize(float), Some(FloatState::Normal));
    assert_eq!(outer(&set, float, &SINGLE), Rect::new(200, 100, 400, 300));
}

#[test]
fn tc_014_maximized_float_refuses_drags_and_collapse_returns_to_it() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(200, 100, 400, 300));
    set.toggle_maximize(float);

    set.toggle_collapse(float);
    assert_eq!(set.toggle_collapse(float), Some(FloatState::Maximized));

    assert!(!drag(
        &mut set,
        float,
        FloatDragKind::Move,
        Point::new(10, 10),
        (50, 50),
        &SINGLE
    ));
    let right = FloatDragKind::Resize(edges(false, true, false, false));
    assert!(!drag(
        &mut set,
        float,
        right,
        Point::new(999, 300),
        (-50, 0),
        &SINGLE
    ));
    assert_eq!(outer(&set, float, &SINGLE), AREA);

    // (d) a normal float, collapsed, ignores maximize and expands to normal.
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(200, 100, 400, 300));
    set.toggle_collapse(float);
    assert_eq!(set.toggle_maximize(float), Some(FloatState::Collapsed));
    assert_eq!(set.toggle_collapse(float), Some(FloatState::Normal));
}

#[test]
fn tc_015_focus_cycling_follows_history_then_creation_order_and_skips_collapsed() {
    let mut set = FloatSet::default();
    let f1 = id(1);
    let f2 = id(2);
    let f3 = id(3);
    for float in [f1, f2, f3] {
        set.create(float, B, &layout(&SPLIT), M).unwrap();
        set.note_focus(float);
    }
    set.toggle_collapse(f3);
    set.note_focus(f1);
    let l = layout(&SPLIT);

    // From a split pane: the last-focused target, not the newest.
    assert_eq!(set.toggle_focus_target(&l, M), Some(f1));
    assert_eq!(set.cycle_from(Some(f1), true, &l, M), Some(f2));
    assert_eq!(set.cycle_from(Some(f2), true, &l, M), Some(f1));
    assert_eq!(set.cycle_from(Some(f1), false, &l, M), Some(f2));
    // From a split pane: first / last target.
    assert_eq!(set.cycle_from(None, true, &l, M), Some(f1));
    assert_eq!(set.cycle_from(None, false, &l, M), Some(f2));

    // A point in B clear of every float and of the divider's hit zone.
    assert_eq!(set.hit_test(Point::new(520, 10), &l, M), None);
    let content = set.placed(f1, &l, M).unwrap().content.unwrap();
    let inside = Point::new(content.x + content.w / 2, content.y + content.h / 2);
    assert_eq!(set.hit_test(inside, &l, M).map(|hit| hit.id), Some(f1));
}

#[test]
fn tc_020_toggle_focus_without_history_picks_the_newest_target() {
    let mut set = FloatSet::default();
    let record = |seq, collapsed| FloatRecord {
        origin: (0.2, 0.2),
        size: (0.3, 0.4),
        collapsed,
        seq,
        z: seq,
        cwd: None,
        strip_moved: false,
    };
    set.restore(id(1), FloatAnchor::Pane(B), &record(1, false));
    set.restore(id(2), FloatAnchor::Tab, &record(2, true));
    set.restore(id(3), FloatAnchor::Window, &record(3, false));
    assert_eq!(set.toggle_focus_target(&layout(&SPLIT), M), Some(id(3)));
    assert_eq!(set.get(id(2)).unwrap().state, FloatState::Collapsed);
    assert!(!set.get(id(2)).unwrap().maximized_before_collapse);
}

#[test]
fn tc_020_records_round_trip_geometry_order_and_collapse() {
    let mut set = FloatSet::default();
    let pane_float = float_at(&mut set, 1, B, &SPLIT, Rect::new(600, 100, 300, 200));
    let tab_float = float_at(&mut set, 2, A, &SPLIT, Rect::new(100, 200, 300, 200));
    set.cycle_anchor(tab_float, &layout(&SPLIT), M).unwrap();
    set.toggle_maximize(tab_float);
    set.toggle_collapse(tab_float);

    let mut restored = FloatSet::default();
    for (anchor, new_id) in [(FloatAnchor::Pane(B), id(11)), (FloatAnchor::Tab, id(12))] {
        for record in set.records_on(anchor, |_| None) {
            assert!(record.is_valid());
            restored.restore(new_id, anchor, &record);
        }
    }
    assert_eq!(
        outer(&restored, id(11), &SPLIT),
        outer(&set, pane_float, &SPLIT)
    );
    assert_eq!(restored.get(id(12)).unwrap().state, FloatState::Collapsed);
    assert_eq!(restored.toggle_collapse(id(12)), Some(FloatState::Normal));
    assert_eq!(
        outer(&restored, id(12), &SPLIT),
        Rect::new(100, 200, 300, 200)
    );
    let order: Vec<PaneId> = restored.floats().iter().map(|float| float.id).collect();
    assert_eq!(order, vec![id(11), id(12)]);
}

#[test]
fn tc_021_record_with_a_non_finite_or_out_of_range_field_is_invalid() {
    let good = FloatRecord {
        origin: (0.1, 0.1),
        size: (0.5, 0.5),
        collapsed: false,
        seq: 1,
        z: 1,
        cwd: None,
        strip_moved: false,
    };
    assert!(good.is_valid());
    for bad in [
        FloatRecord {
            origin: (f64::NAN, 0.1),
            ..good.clone()
        },
        FloatRecord {
            origin: (0.1, 1.5),
            ..good.clone()
        },
        FloatRecord {
            size: (0.0, 0.5),
            ..good.clone()
        },
        FloatRecord {
            size: (0.5, f64::INFINITY),
            ..good.clone()
        },
    ] {
        assert!(!bad.is_valid());
    }
}

#[test]
fn tc_022_ninth_float_and_a_switch_onto_a_full_anchor_are_refused() {
    let mut set = FloatSet::default();
    for raw in 1..=8 {
        set.create(id(raw), A, &layout(&SINGLE), M).unwrap();
    }
    assert_eq!(set.create(id(9), A, &layout(&SINGLE), M), Err(AnchorFull));
    assert_eq!(set.count_on(FloatAnchor::Pane(A)), 8);

    // (b) eight tab floats, then a pane float that cannot join them.
    let mut set = FloatSet::default();
    for raw in 1..=8 {
        set.create(id(raw), A, &layout(&SINGLE), M).unwrap();
        set.cycle_anchor(id(raw), &layout(&SINGLE), M).unwrap();
    }
    set.create(id(9), A, &layout(&SINGLE), M).unwrap();
    assert_eq!(
        set.cycle_anchor(id(9), &layout(&SINGLE), M),
        Err(AnchorFull)
    );
    assert_eq!(set.get(id(9)).unwrap().anchor, FloatAnchor::Pane(A));
}

#[test]
fn tc_023_empty_set_offers_no_targets() {
    let set = FloatSet::default();
    let l = layout(&SINGLE);
    assert_eq!(set.toggle_focus_target(&l, M), None);
    assert_eq!(set.cycle_from(None, true, &l, M), None);
    assert_eq!(set.cycle_from(None, false, &l, M), None);
    assert_eq!(set.action_target(None, &l, M), None);
}

#[test]
fn tc_026_resize_respects_minimum_anchor_and_hit_priority() {
    let start = Rect::new(300, 100, 400, 300);
    let resized = |edges: Edges, from: Point, by: (i64, i64)| -> Rect {
        let mut set = FloatSet::default();
        let float = float_at(&mut set, 1, A, &SINGLE, start);
        assert!(drag(
            &mut set,
            float,
            FloatDragKind::Resize(edges),
            from,
            by,
            &SINGLE
        ));
        outer(&set, float, &SINGLE)
    };
    let corner = Point::new(699, 399);

    // (a) shrink to the 20×3-cell content minimum.
    let small = resized(edges(false, true, false, true), corner, (-2000, -2000));
    assert_eq!((small.w, small.h - M.strip_h()), (200, 60));
    // (b) stop at the anchor's far corner.
    let big = resized(edges(false, true, false, true), corner, (2000, 2000));
    assert_eq!((big.right(), big.bottom()), (1000, 600));
    // (c) left edge out by 50 px: five more columns.
    let wider = resized(
        edges(true, false, false, false),
        Point::new(300, 250),
        (-50, 0),
    );
    assert_eq!((wider.x, wider.w, wider.w / M.cell_w), (250, 450, 45));

    // (d) each of the eight handles moves only its own edges.
    for (left, right, top, bottom) in [
        (true, false, false, false),
        (false, true, false, false),
        (false, false, true, false),
        (false, false, false, true),
        (true, false, true, false),
        (false, true, true, false),
        (true, false, false, true),
        (false, true, false, true),
    ] {
        let by = (
            if left {
                -10
            } else if right {
                10
            } else {
                0
            },
            if top {
                -10
            } else if bottom {
                10
            } else {
                0
            },
        );
        let rect = resized(edges(left, right, top, bottom), Point::new(500, 250), by);
        assert_eq!(rect.x, start.x - if left { 10 } else { 0 });
        assert_eq!(rect.right(), start.right() + if right { 10 } else { 0 });
        assert_eq!(rect.y, start.y - if top { 10 } else { 0 });
        assert_eq!(rect.bottom(), start.bottom() + if bottom { 10 } else { 0 });
    }

    // (e) a tab float whose left edge sits on the A/B divider: 3 px outside
    // the edge still hits the float's resize zone.
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, B, &SPLIT, Rect::new(500, 100, 400, 300));
    set.cycle_anchor(float, &layout(&SPLIT), M).unwrap();
    let press = Point::new(497, 200);
    let hit = set.hit_test(press, &layout(&SPLIT), M).unwrap();
    assert_eq!(
        hit.part,
        FloatHitPart::Resize(edges(true, false, false, false))
    );
    assert!(drag(
        &mut set,
        float,
        FloatDragKind::Resize(edges(true, false, false, false)),
        press,
        (-50, 0),
        &SPLIT
    ));
    assert_eq!(outer(&set, float, &SPLIT), Rect::new(450, 100, 450, 300));

    // (f) a front float's content wins over a back float's resize zone.
    let mut set = FloatSet::default();
    let back = float_at(&mut set, 1, A, &SINGLE, Rect::new(100, 100, 300, 300));
    let front = float_at(&mut set, 2, A, &SINGLE, Rect::new(350, 150, 300, 200));
    let hit = set
        .hit_test(Point::new(402, 250), &layout(&SINGLE), M)
        .unwrap();
    assert_eq!((hit.id, hit.part), (front, FloatHitPart::Content));
    assert_eq!(outer(&set, back, &SINGLE), Rect::new(100, 100, 300, 300));
}

#[test]
fn tc_033_hidden_floats_are_absent_from_the_draw_order() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SPLIT, Rect::new(50, 100, 300, 200));
    let zoomed = FloatLayout {
        zoomed: Some(B),
        ..layout(&SPLIT)
    };
    assert!(set.draw_order(&zoomed, M).is_empty());
    let tiny = [(A, Rect::new(0, 0, 150, 60))];
    assert!(set.draw_order(&layout(&tiny), M).is_empty());
    assert_eq!(set.draw_order(&layout(&SPLIT), M).len(), 1);
    assert!(set.contains(float));
}

#[test]
fn tc_039_collapsed_float_moves_by_its_strip_and_clamps_on_expand() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(500, 250, 400, 300));
    set.note_focus(float);
    set.toggle_collapse(float);

    assert!(drag(
        &mut set,
        float,
        FloatDragKind::Move,
        Point::new(550, 255),
        (900, 900),
        &SINGLE
    ));
    assert_eq!(outer(&set, float, &SINGLE), Rect::new(600, 580, 400, 20));

    let right = FloatDragKind::Resize(edges(false, true, false, false));
    assert!(!drag(
        &mut set,
        float,
        right,
        Point::new(999, 585),
        (-50, 0),
        &SINGLE
    ));

    // (c) the action targets the last-focused float although none has focus.
    let target = set.action_target(None, &layout(&SINGLE), M).unwrap();
    assert_eq!(target, float);
    assert_eq!(
        set.cycle_anchor(target, &layout(&SINGLE), M),
        Ok(Some(FloatAnchor::Tab))
    );

    set.toggle_collapse(float);
    let expanded = outer(&set, float, &SINGLE);
    assert_eq!((expanded.x, expanded.y), (600, 300));
}

#[test]
fn tc_040_cycling_a_maximized_float_keeps_it_maximized_on_the_new_anchor() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SPLIT, Rect::new(0, 100, 400, 500));
    set.cycle_anchor(float, &layout(&SPLIT), M).unwrap();
    set.cycle_anchor(float, &layout(&SPLIT), M).unwrap();
    let window = layout(&SPLIT).anchor_rect(FloatAnchor::Window).unwrap();
    set_from_rect(
        set.get_mut(float).unwrap(),
        Rect::new(0, 100, 800, 500),
        window,
    );
    set.toggle_maximize(float);

    assert_eq!(
        set.cycle_anchor(float, &layout(&SPLIT), M),
        Ok(Some(FloatAnchor::Pane(A)))
    );
    assert_eq!(outer(&set, float, &SPLIT), Rect::new(0, 0, 500, 600));
    set.toggle_maximize(float);
    assert_eq!(outer(&set, float, &SPLIT), Rect::new(0, 100, 500, 500));
}

#[test]
fn tc_042_strip_parts_are_hit_as_buttons_or_title() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(200, 100, 400, 300));
    let strip = set.placed(float, &layout(&SINGLE), M).unwrap().strip;
    let centre = |rect: Rect| Point::new(rect.x + rect.w / 2, rect.y + rect.h / 2);
    for button in [
        StripButton::Anchor,
        StripButton::Maximize,
        StripButton::Collapse,
        StripButton::Close,
    ] {
        let hit = set
            .hit_test(
                centre(strip_button_rect(strip, button, M)),
                &layout(&SINGLE),
                M,
            )
            .unwrap();
        assert_eq!(hit.part, FloatHitPart::Button(button));
    }
    let title = set.hit_test(centre(strip), &layout(&SINGLE), M).unwrap();
    assert_eq!(title.part, FloatHitPart::Strip);
}

#[test]
fn tc_044_font_size_change_keeps_the_rect_and_recomputes_the_grid() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(200, 100, 400, 300));
    let grid = |metrics: FloatMetrics| {
        let placed = set.placed(float, &layout(&SINGLE), metrics).unwrap();
        let content = placed.content.unwrap();
        (
            placed.outer,
            content.w / metrics.cell_w,
            content.h / metrics.cell_h,
        )
    };
    assert_eq!(grid(M), (Rect::new(200, 100, 400, 300), 40, 14));
    let large = FloatMetrics {
        cell_w: 20,
        cell_h: 40,
        ..M
    };
    assert_eq!(grid(large), (Rect::new(200, 100, 400, 300), 20, 6));
}

// Regression tests for the review of the first floating-panes change.

#[test]
fn review_4_resize_drag_survives_an_anchor_that_moved_past_the_start_rect() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(50, 100, 300, 200));
    set.cycle_anchor(float, &layout(&SINGLE), M).unwrap();
    let right = FloatDragKind::Resize(edges(false, true, false, false));
    let held = set
        .begin_drag(float, right, Point::new(349, 200), &layout(&SINGLE), M)
        .unwrap();

    // A sidebar opens mid-drag: the tab area now starts at x = 360.
    let shifted = Rect::new(360, 0, 640, 600);
    let panes = [(A, shifted)];
    let moved = FloatLayout {
        tab: shifted,
        window: shifted,
        panes: &panes,
        zoomed: None,
    };
    set.drag_to(&held, Point::new(700, 200), &moved, M);
    let rect = set.placed(float, &moved, M).unwrap().outer;
    assert!(rect.x >= 360 && rect.right() <= 1000, "{rect:?}");
    let origin = set.get(float).unwrap().origin;
    assert!((0.0..=1.0).contains(&origin.0), "{origin:?}");
}

#[test]
fn review_7_adopted_floats_keep_their_stacking_order() {
    let mut tab1 = FloatSet::default();
    let w1 = float_at(&mut tab1, 1, A, &SINGLE, Rect::new(100, 100, 300, 200));
    let w2 = float_at(&mut tab1, 2, A, &SINGLE, Rect::new(150, 150, 300, 200));
    for float in [w1, w2] {
        tab1.cycle_anchor(float, &layout(&SINGLE), M).unwrap();
        tab1.cycle_anchor(float, &layout(&SINGLE), M).unwrap();
    }
    tab1.note_focus(w1);
    let front = |set: &FloatSet| set.draw_order(&layout(&SINGLE), M).last().unwrap().id;
    assert_eq!(front(&tab1), w1);

    let mut tab2 = FloatSet::default();
    tab2.adopt(tab1.take_anchor(FloatAnchor::Window));
    assert_eq!(front(&tab2), w1);
    // Creation order, which focus cycling follows, is kept too.
    let order: Vec<PaneId> = tab2.floats().iter().map(|float| float.id).collect();
    assert_eq!(order, vec![w1, w2]);
}

#[test]
fn review_1_last_split_pane_takes_the_tabs_floats_with_it() {
    let mut set = FloatSet::default();
    let pane_float = float_at(&mut set, 1, A, &SPLIT, Rect::new(10, 10, 300, 200));
    let other_pane_float = float_at(&mut set, 2, B, &SPLIT, Rect::new(600, 10, 300, 200));
    let tab_float = float_at(&mut set, 3, A, &SPLIT, Rect::new(50, 300, 300, 200));
    let window_float = float_at(&mut set, 4, A, &SPLIT, Rect::new(100, 350, 300, 200));
    set.cycle_anchor(tab_float, &layout(&SPLIT), M).unwrap();
    set.cycle_anchor(window_float, &layout(&SPLIT), M).unwrap();
    set.cycle_anchor(window_float, &layout(&SPLIT), M).unwrap();

    // One of several split panes: only its own floats.
    assert_eq!(set.closing_with_pane(A, false, true), vec![pane_float]);
    // The tab's last split pane closes the tab: every float of the tab
    // goes, except window floats another tab will take over.
    let mut single = FloatSet::default();
    let p = float_at(&mut single, 1, A, &SINGLE, Rect::new(10, 10, 300, 200));
    let t = float_at(&mut single, 2, A, &SINGLE, Rect::new(50, 300, 300, 200));
    let w = float_at(&mut single, 3, A, &SINGLE, Rect::new(100, 350, 300, 200));
    single.cycle_anchor(t, &layout(&SINGLE), M).unwrap();
    single.cycle_anchor(w, &layout(&SINGLE), M).unwrap();
    single.cycle_anchor(w, &layout(&SINGLE), M).unwrap();
    assert_eq!(single.closing_with_pane(A, true, true), vec![p, t]);
    assert_eq!(single.closing_with_pane(A, true, false), vec![p, t, w]);
    let _ = other_pane_float;
}

#[test]
fn review_8_tab_close_spares_window_floats_another_tab_takes_over() {
    let mut set = FloatSet::default();
    let tab_float = float_at(&mut set, 1, A, &SINGLE, Rect::new(50, 300, 300, 200));
    let window_float = float_at(&mut set, 2, A, &SINGLE, Rect::new(100, 350, 300, 200));
    set.cycle_anchor(tab_float, &layout(&SINGLE), M).unwrap();
    set.cycle_anchor(window_float, &layout(&SINGLE), M).unwrap();
    set.cycle_anchor(window_float, &layout(&SINGLE), M).unwrap();
    assert_eq!(set.closing_with_tab(true), vec![tab_float]);
    assert_eq!(set.closing_with_tab(false), vec![tab_float, window_float]);
}

#[test]
fn review2_4_a_round_trip_between_tabs_keeps_creation_order() {
    let mut tab_a = FloatSet::default();
    let w = float_at(&mut tab_a, 1, A, &SINGLE, Rect::new(100, 100, 300, 200));
    tab_a.cycle_anchor(w, &layout(&SINGLE), M).unwrap();
    tab_a.cycle_anchor(w, &layout(&SINGLE), M).unwrap();
    let t = float_at(&mut tab_a, 2, A, &SINGLE, Rect::new(150, 150, 300, 200));
    tab_a.cycle_anchor(t, &layout(&SINGLE), M).unwrap();

    let mut tab_b = FloatSet::default();
    tab_b.adopt(tab_a.take_anchor(FloatAnchor::Window));
    tab_a.adopt(tab_b.take_anchor(FloatAnchor::Window));

    let order: Vec<PaneId> = tab_a.floats().iter().map(|float| float.id).collect();
    assert_eq!(order, vec![w, t]);
    assert_eq!(tab_a.cycle_targets(&layout(&SINGLE), M), vec![w, t]);
}

#[test]
fn review2_5_focus_history_stays_comparable_across_tabs() {
    let mut tab_a = FloatSet::default();
    let w = float_at(&mut tab_a, 1, A, &SINGLE, Rect::new(100, 100, 300, 200));
    tab_a.cycle_anchor(w, &layout(&SINGLE), M).unwrap();
    tab_a.cycle_anchor(w, &layout(&SINGLE), M).unwrap();
    for _ in 0..5 {
        tab_a.note_focus(w);
    }

    let mut tab_b = FloatSet::default();
    let p = float_at(&mut tab_b, 2, A, &SINGLE, Rect::new(400, 300, 300, 200));
    tab_b.note_focus(p);
    tab_b.adopt(tab_a.take_anchor(FloatAnchor::Window));
    // The user focuses P after W arrived: P is the last-focused float.
    tab_b.note_focus(p);

    let l = layout(&SINGLE);
    assert_eq!(tab_b.toggle_focus_target(&l, M), Some(p));
    assert_eq!(tab_b.action_target(None, &l, M), Some(p));
}

// Third review round.

#[test]
fn review3_1_the_last_split_pane_takes_the_tabs_floats_to_its_new_tab() {
    assert_eq!(
        FloatSet::anchors_following_pane(A, false, true),
        vec![FloatAnchor::Pane(A)]
    );
    // The tab closes behind its last split pane: its tab floats go along,
    // and its window floats too when no other tab remains to hold them.
    assert_eq!(
        FloatSet::anchors_following_pane(A, true, true),
        vec![FloatAnchor::Pane(A), FloatAnchor::Tab]
    );
    assert_eq!(
        FloatSet::anchors_following_pane(A, true, false),
        vec![FloatAnchor::Pane(A), FloatAnchor::Tab, FloatAnchor::Window]
    );
}

#[test]
fn review3_2_strip_buttons_are_hit_where_they_are_drawn() {
    // Cell 10 px, 24 px of left padding and 24 px of right: a 400 px strip
    // draws 35 columns from x = strip.x + 24.
    let padded = FloatMetrics {
        pad_w: 48,
        pad_left: 24,
        ..M
    };
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(200, 100, 400, 300));
    let l = layout(&SINGLE);
    let strip = set.placed(float, &l, padded).unwrap().strip;
    let part = |x: u32| {
        set.hit_test(Point::new(x, strip.y + strip.h / 2), &l, padded)
            .unwrap()
            .part
    };
    // The anchor icon is drawn in column 0, at x 224..234.
    assert_eq!(
        part(strip.x + 29),
        FloatHitPart::Button(StripButton::Anchor)
    );
    // Title text starts at column 2.
    assert_eq!(part(strip.x + 24 + 60), FloatHitPart::Strip);
    // Columns 29-30 maximize, 31-32 collapse, 33-34 close; the glyph sits in
    // the second column of each pair.
    let col = |c: u32| strip.x + 24 + c * 10 + 5;
    assert_eq!(part(col(30)), FloatHitPart::Button(StripButton::Maximize));
    assert_eq!(part(col(32)), FloatHitPart::Button(StripButton::Collapse));
    assert_eq!(part(col(34)), FloatHitPart::Button(StripButton::Close));
    // Past the last column, the padding still belongs to the close button.
    assert_eq!(
        part(strip.right() - 8),
        FloatHitPart::Button(StripButton::Close)
    );
}

#[test]
fn review3_9_collapsing_a_minimum_sized_float_keeps_its_top_edge() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(100, 480, 400, 120));
    let small = [(A, Rect::new(0, 0, 1000, 300))];
    let small_layout = FloatLayout {
        tab: small[0].1,
        window: small[0].1,
        panes: &small,
        zoomed: None,
    };
    let normal = set.placed(float, &small_layout, M).unwrap().outer;
    assert_eq!((normal.y, normal.h), (220, 80));
    set.toggle_collapse(float);
    let strip = set.placed(float, &small_layout, M).unwrap().outer;
    assert_eq!(
        (strip.x, strip.y, strip.w, strip.h),
        (normal.x, 220, normal.w, 20)
    );
}

// Fourth review round.

#[test]
fn review4_3_cycling_the_anchor_keeps_a_dragged_collapsed_strip_in_place() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(500, 250, 400, 300));
    set.toggle_collapse(float);
    assert!(drag(
        &mut set,
        float,
        FloatDragKind::Move,
        Point::new(550, 255),
        (900, 900),
        &SINGLE
    ));
    assert_eq!(outer(&set, float, &SINGLE), Rect::new(600, 580, 400, 20));

    set.cycle_anchor(float, &layout(&SINGLE), M).unwrap();
    assert_eq!(outer(&set, float, &SINGLE), Rect::new(600, 580, 400, 20));
    // Expanding still brings the full rect back inside the anchor.
    set.toggle_collapse(float);
    assert_eq!(outer(&set, float, &SINGLE), Rect::new(600, 300, 400, 300));
}

#[test]
fn review4_4_a_dragged_collapsed_strip_restores_where_it_was_saved() {
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(500, 250, 400, 300));
    set.toggle_collapse(float);
    assert!(drag(
        &mut set,
        float,
        FloatDragKind::Move,
        Point::new(550, 255),
        (900, 900),
        &SINGLE
    ));
    let saved = outer(&set, float, &SINGLE);
    assert_eq!(saved, Rect::new(600, 580, 400, 20));

    let mut restored = FloatSet::default();
    for record in set.records_on(FloatAnchor::Pane(A), |_| None) {
        restored.restore(id(9), FloatAnchor::Pane(A), &record);
    }
    assert_eq!(outer(&restored, id(9), &SINGLE), saved);

    // A strip that was never dragged still restores at the float's corner.
    let mut set = FloatSet::default();
    let float = float_at(&mut set, 1, A, &SINGLE, Rect::new(100, 480, 400, 120));
    set.toggle_collapse(float);
    let small = [(A, Rect::new(0, 0, 1000, 300))];
    let small_layout = FloatLayout {
        tab: small[0].1,
        window: small[0].1,
        panes: &small,
        zoomed: None,
    };
    let mut restored = FloatSet::default();
    for record in set.records_on(FloatAnchor::Pane(A), |_| None) {
        restored.restore(id(9), FloatAnchor::Pane(A), &record);
    }
    assert_eq!(
        restored.placed(id(9), &small_layout, M).unwrap().outer.y,
        220
    );
}
