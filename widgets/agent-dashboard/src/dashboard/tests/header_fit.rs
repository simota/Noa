//! AC4 (header): the header fits from 1100 to 1920 px, and drops its items
//! in the order the spec gives (dashboard-worker-list U3: the グループ
//! caption, then the title; no counts, 最長 or chips are left to drop).

use std::time::Instant;

use noa_gpui_text::TextStyle;

use crate::dashboard::header::STATUS_MAX_WIDTH;

use super::*;

const WIDTHS: [f32; 5] = [1100.0, 1280.0, 1440.0, 1680.0, 1920.0];

/// The states the header must fit in.
#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Default,
    /// The filter focused, holding "perf": the match count shows too, the
    /// worst case.
    Filtering,
    /// A status text 300 characters long (a notice).
    LongStatus,
    /// Down with a 200-character reason.
    LongReason,
}

const STATES: [State; 4] = [
    State::Default,
    State::Filtering,
    State::LongStatus,
    State::LongReason,
];

fn size(width: f32) -> Size {
    Size::new(width, 900.0)
}

/// A dashboard in `state` at `width`, laid out.
fn at(width: f32, state: State) -> (Dashboard, TextStore) {
    let mut dashboard = Dashboard::new(Grouping::Orchestrator);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(size(width), &mut text);
    let now = Instant::now();
    match state {
        State::Filtering => {
            let dispatch = |dashboard: &mut Dashboard, text: &mut TextStore, event| {
                dashboard.dispatch(event, text);
                dashboard.prepare(size(width), text);
            };
            let key = Event::KeyDown {
                key: Key::Char('/'),
                modifiers: Modifiers::default(),
            };
            dispatch(&mut dashboard, &mut text, key);
            // The platform sends the slash as text after its key; the key
            // map has already taken it.
            dispatch(&mut dashboard, &mut text, Event::Text("/".to_owned()));
            dispatch(&mut dashboard, &mut text, Event::Text("perf".to_owned()));
            assert_eq!(dashboard.filter.query, "perf");
        }
        State::LongStatus => {
            dashboard.show_notice_at("あ".repeat(300), now);
        }
        State::LongReason => lose_link(&mut dashboard, &mut text, now),
        State::Default => {}
    }
    dashboard.prepare(size(width), &mut text);
    (dashboard, text)
}

/// The reason of `lose_link`'s `Down`: 200 characters.
fn long_reason() -> String {
    "切".repeat(200)
}

/// The link goes down with a 200-character reason while the canned workers
/// are shown (they stay, marked stale).
fn lose_link(dashboard: &mut Dashboard, text: &mut TextStore, now: Instant) {
    let snapshot = Snapshot {
        fleet: Fleet::default(),
        link: Link::Down(long_reason()),
        synced_secs: 0,
        can_focus: false,
        can_input: false,
        safe_send: true,
        instance: None,
    };
    dashboard.apply_received(snapshot, now, text);
    dashboard.tick(now);
}

/// The width a text takes in `style`, measured on its own: not by the
/// header, whose fit this checks.
fn text_width(text: &mut TextStore, s: &str, style: TextStyle) -> f32 {
    let id = text.create(s, style, None);
    let width = text.size(id).width;
    text.remove(id);
    width
}

/// The status label's whole text at its natural width (12 px, as the
/// header writes it).
fn status_natural(dashboard: &Dashboard, text: &mut TextStore) -> f32 {
    let label = dashboard.tree.get::<Label>(dashboard.status).unwrap();
    text_width(text, label.text(), TextStyle::new(12.0))
}

/// The header bar's own children that are shown, with their rectangles.
fn shown_children(dashboard: &Dashboard) -> Vec<(WidgetId, [f32; 4])> {
    let tree = &dashboard.tree;
    tree.children(dashboard.header.bar)
        .iter()
        .filter(|id| !tree.is_hidden(**id))
        .map(|id| (*id, rect(tree, *id)))
        .collect()
}

fn width_of(dashboard: &Dashboard, id: WidgetId) -> f32 {
    rect(&dashboard.tree, id)[2]
}

fn shown(dashboard: &Dashboard, id: WidgetId) -> bool {
    !dashboard.tree.is_hidden(id)
}

#[test]
fn the_header_fits_from_1100_to_1920() {
    for width in WIDTHS {
        for state in STATES {
            let (dashboard, mut text) = at(width, state);
            let what = format!("{width} px, {state:?}");
            no_overlap(&dashboard, width, &what);

            // What is never dropped is there, with a width.
            let header = &dashboard.header;
            let slot = dashboard.tree.parent(header.health.status).unwrap();
            let never = [
                ("dot", header.health.dot),
                ("status slot", slot),
                ("filter field", header.filter_field),
                ("? keycap", header.help_key),
                ("grouping control", header.grouping_control),
            ];
            for (name, id) in never {
                assert!(shown(&dashboard, id), "{what}: {name} is hidden");
                assert!(
                    width_of(&dashboard, id) > 0.0,
                    "{what}: {name} has no width"
                );
            }
            // The spec's floor (AC4), written out rather than read from
            // the header: 140 px, or the whole text when it is shorter.
            let floor = 140.0_f32.min(status_natural(&dashboard, &mut text));
            assert!(
                width_of(&dashboard, slot) >= floor - 0.5,
                "{what}: the status slot is {} wide, under {floor}",
                width_of(&dashboard, slot)
            );
        }
    }
}

#[test]
fn nothing_is_dropped_at_1440() {
    let (dashboard, _text) = at(1440.0, State::Default);
    let tree = &dashboard.tree;
    // The match count is the one child hidden by design: with no filter
    // there is nothing to count.
    let hidden: Vec<WidgetId> = descendants(tree, dashboard.header.bar)
        .into_iter()
        .filter(|id| tree.is_hidden(*id) && *id != dashboard.header.match_count)
        .collect();
    assert!(hidden.is_empty(), "hidden at 1440: {hidden:?}");
}

fn descendants(tree: &UiTree, id: WidgetId) -> Vec<WidgetId> {
    tree.children(id)
        .iter()
        .flat_map(|child| std::iter::once(*child).chain(descendants(tree, *child)))
        .collect()
}

/// Which of the two droppable items are hidden, in drop order: the
/// グループ caption, the title.
fn dropped(dashboard: &Dashboard) -> [bool; 2] {
    let header = &dashboard.header;
    [
        !shown(dashboard, header.group_caption),
        !shown(dashboard, header.title),
    ]
}

/// The narrowest width the filtering header fits at with nothing dropped,
/// measured: the drop tests derive their widths from it, not from the
/// fonts of one platform.
fn widest_drop() -> u32 {
    let (mut dashboard, mut text) = at(1920.0, State::Filtering);
    let mut width = 1920;
    while dropped(&dashboard) == [false; 2] {
        assert!(width > 400, "nothing drops down to {width} px");
        width -= 1;
        dashboard.prepare(size(width as f32), &mut text);
    }
    width
}

#[test]
fn the_drop_order_is_kept() {
    // Narrowing one window from 1920 px in steps of 4 until both items
    // went (or the never-dropped items alone fill the bar): each item goes
    // only after the one before it and none comes back; the header fits at
    // every width while anything is left to drop.
    let (mut dashboard, mut text) = at(1920.0, State::Filtering);
    let mut previous = dropped(&dashboard);
    assert_eq!(previous, [false; 2], "1920 px drops nothing");
    let mut width = 1916;
    while previous != [true; 2] && width > 400 {
        dashboard.prepare(size(width as f32), &mut text);
        let now = dropped(&dashboard);
        for (before, after) in previous.iter().zip(&now) {
            assert!(!before || *after, "{width}: an item came back: {now:?}");
        }
        assert!(now[0] || !now[1], "{width}: the title went first: {now:?}");
        if now != [true; 2] {
            no_overlap(&dashboard, width as f32, &format!("{width} px narrowing"));
        }
        previous = now;
        width -= 4;
    }
    assert_eq!(previous, [true; 2], "both go before 400 px");
}

/// The shown children of the bar neither overlap nor leave [16, width - 16].
fn no_overlap(dashboard: &Dashboard, width: f32, what: &str) {
    let mut children = shown_children(dashboard);
    children.sort_by(|a, b| a.1[0].total_cmp(&b.1[0]));
    for pair in children.windows(2) {
        let (left, right) = (pair[0].1, pair[1].1);
        assert!(
            left[0] + left[2] <= right[0] + 0.5,
            "{what}: {left:?} overlaps {right:?}"
        );
    }
    for (_, r) in &children {
        assert!(
            r[0] >= 16.0 - 0.5 && r[0] + r[2] <= width - 16.0 + 0.5,
            "{what}: {r:?} leaves [16, {}]",
            width - 16.0
        );
    }
}

#[test]
fn dropped_items_come_back_when_the_window_widens() {
    let narrow = widest_drop() as f32 - 40.0;
    let (mut dashboard, mut text) = at(narrow, State::Filtering);
    let caption = dashboard.header.group_caption;
    assert!(!shown(&dashboard, caption));
    dashboard.prepare(size(1920.0), &mut text);
    dashboard.prepare(size(1920.0), &mut text);
    assert!(shown(&dashboard, caption));
    assert!(shown(&dashboard, dashboard.header.title));
    // And it settles: laying out again changes nothing.
    assert!(!dashboard.prepare(size(1920.0), &mut text));

    // Widening one window to 1920 px: items come back in the reverse
    // order and none goes again, and the header fits throughout.
    let (mut dashboard, mut text) = at(narrow, State::Filtering);
    let mut previous = dropped(&dashboard);
    for width in (narrow as u32 + 4..=1920).step_by(4) {
        dashboard.prepare(size(width as f32), &mut text);
        let now = dropped(&dashboard);
        for (before, after) in previous.iter().zip(&now) {
            assert!(*before || !after, "{width}: an item went again: {now:?}");
        }
        no_overlap(&dashboard, width as f32, &format!("{width} px widening"));
        previous = now;
    }
    assert_eq!(previous, [false; 2]);
}

#[test]
fn the_tally_tags_and_the_hint_strip_are_gone() {
    let (dashboard, _text) = laid_out(Grouping::Orchestrator);
    let tags = tags_under(&dashboard.tree, dashboard.header.bar);
    // No tag at all: the 最長 tag went with the urgency counts.
    assert!(tags.is_empty(), "{tags:?}");
}

/// The states a lost link must stay readable in: with the filter's count
/// and without.
const LAYOUTS: [State; 2] = [State::Default, State::Filtering];

#[test]
fn a_lost_link_keeps_its_age_readable_at_every_width() {
    // What must show however long the reason is: the link state and the
    // data's age, then the reason's start.
    let fixed = "切断: 最後の受信から 0 秒 · ";
    for state in LAYOUTS {
        for width in (1100..=1920).step_by(20) {
            let (mut dashboard, mut text) = at(width as f32, state);
            lose_link(&mut dashboard, &mut text, Instant::now());
            dashboard.prepare(size(width as f32), &mut text);
            let label = dashboard.tree.get::<Label>(dashboard.status).unwrap();
            let shown = label.shown_text();
            let what = format!("{width} px, {state:?}");
            let reason = shown
                .strip_prefix(fixed)
                .unwrap_or_else(|| panic!("{what}: {shown:?} cuts the fixed part"));
            assert!(
                reason.starts_with('切') && reason.ends_with('…'),
                "{what}: the reason's start is not shown: {shown:?}"
            );
            assert_eq!(label.text(), format!("{fixed}{}", long_reason()));
        }
    }
}

#[test]
fn a_stale_link_keeps_its_age_readable_at_every_width() {
    // What must show however the slot is squeezed: the data's age, from
    // seconds to many minutes. 古いデータ after it may be cut.
    let ages = [
        (10, "10 秒"),
        (59, "59 秒"),
        (59 * 60, "59 分"),
        (999 * 60, "999 分"),
    ];
    let now = Instant::now();
    for state in LAYOUTS {
        for width in (1100..=1920).step_by(20) {
            let (mut dashboard, mut text) = at(width as f32, state);
            for (synced, age) in ages {
                dashboard.apply_received(live(fleet::canned(), synced), now, &mut text);
                dashboard.tick(now);
                dashboard.prepare(size(width as f32), &mut text);
                let label = dashboard.tree.get::<Label>(dashboard.status).unwrap();
                let fixed = format!("最後の受信から {age}");
                assert_eq!(label.text(), format!("{fixed}（古いデータ）"));
                let shown = label.shown_text();
                assert!(
                    shown.starts_with(&fixed),
                    "{width} px, {state:?}, {age}: {shown:?} cuts the age"
                );
            }
        }
    }
}

#[test]
fn the_status_keeps_its_width_until_everything_else_is_dropped() {
    let now = Instant::now();
    for state in LAYOUTS {
        let (mut dashboard, mut text) = at(1920.0, state);
        // 30 s without an answer: a status longer than the slot's maximum.
        dashboard.apply_received(live(fleet::canned(), 30), now, &mut text);
        dashboard.tick(now);
        let widths = (1000..=1920)
            .rev()
            .step_by(4)
            .chain((1000..=1920).step_by(4));
        for width in widths {
            dashboard.prepare(size(width as f32), &mut text);
            let what = format!("{width} px, {state:?}");
            let slot = dashboard
                .tree
                .parent(dashboard.header.health.status)
                .unwrap();
            let wanted = status_natural(&dashboard, &mut text).min(STATUS_MAX_WIDTH);
            if width_of(&dashboard, slot) < wanted - 0.5 {
                // Every droppable item has gone.
                let header = &dashboard.header;
                let all_gone =
                    !shown(&dashboard, header.group_caption) && !shown(&dashboard, header.title);
                assert!(
                    all_gone,
                    "{what}: the status shrank to {} before the rest went: {:?}",
                    width_of(&dashboard, slot),
                    dropped(&dashboard)
                );
            }
        }
    }
}

#[test]
fn the_status_is_measured_as_the_header_lays_it_out() {
    // `status_natural` above is what the label takes when nothing cuts it.
    let (dashboard, mut text) = at(1920.0, State::Default);
    let label = dashboard.header.health.status;
    let natural = status_natural(&dashboard, &mut text);
    assert!(natural > 0.0);
    assert!((width_of(&dashboard, label) - natural).abs() <= 0.5);
}
