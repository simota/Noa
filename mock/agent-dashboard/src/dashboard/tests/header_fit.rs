//! AC4 (header) and AC7 (counts): the header fits from 1100 to 1920 px, and
//! drops its items in the order the spec gives.

use std::time::Instant;

use crate::dashboard::header::STATUS_MIN_WIDTH;

use super::*;

const WIDTHS: [f32; 5] = [1100.0, 1280.0, 1440.0, 1680.0, 1920.0];

/// The states the header must fit in.
#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Default,
    AllHidden,
    /// The filter focused, holding "perf".
    Filtering,
    /// Everything hidden and the filter active: the worst case.
    HiddenAndFiltering,
    /// A status text 300 characters long (a notice).
    LongStatus,
    /// Down with a 200-character reason.
    LongReason,
}

const STATES: [State; 6] = [
    State::Default,
    State::AllHidden,
    State::Filtering,
    State::HiddenAndFiltering,
    State::LongStatus,
    State::LongReason,
];

fn size(width: f32) -> Size {
    Size::new(width, 900.0)
}

/// A dashboard in `state` at `width`, laid out.
fn at(width: f32, state: State) -> (Dashboard, TextStore) {
    let hidden = matches!(state, State::AllHidden | State::HiddenAndFiltering);
    let view = View {
        shown: if hidden { [false; 5] } else { [true; 5] },
        ..View::of(Grouping::Orchestrator)
    };
    let mut dashboard = Dashboard::showing(view);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(size(width), &mut text);
    let now = Instant::now();
    match state {
        State::Filtering | State::HiddenAndFiltering => {
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
        State::LongReason => {
            let reason = "切".repeat(200);
            let snapshot = Snapshot {
                fleet: Fleet::default(),
                link: Link::Down(reason),
                synced_secs: 0,
                can_focus: false,
                instance: None,
            };
            dashboard.apply_received(snapshot, now, &mut text);
            dashboard.tick(now);
        }
        State::Default | State::AllHidden => {}
    }
    dashboard.prepare(size(width), &mut text);
    (dashboard, text)
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
            let (dashboard, _text) = at(width, state);
            let what = format!("{width} px, {state:?}");
            no_overlap(&dashboard, width, &what);

            // What is never dropped is there, with a width.
            let header = &dashboard.header;
            let slot = dashboard.tree.parent(header.health.status).unwrap();
            let never = [
                ("dot", header.health.dot),
                ("status slot", slot),
                ("urgency counts", header.counts),
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
            assert!(
                width_of(&dashboard, slot) >= STATUS_MIN_WIDTH - 0.5,
                "{what}: the status slot is {} wide",
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

#[test]
fn at_1440_with_every_panel_hidden_only_the_caption_goes() {
    let (dashboard, _text) = at(1440.0, State::AllHidden);
    let header = &dashboard.header;
    assert!(!shown(&dashboard, header.group_caption));
    for chip in &header.chip_parts {
        assert!(shown(&dashboard, chip.title));
    }
    assert!(shown(&dashboard, header.title));
    assert!(shown(&dashboard, header.longest.unwrap()));
}

/// Which of the four droppable items are hidden, in drop order: the グループ
/// caption, the chip titles, the 最長 tag, the title.
fn dropped(dashboard: &Dashboard) -> [bool; 4] {
    let header = &dashboard.header;
    [
        !shown(dashboard, header.group_caption),
        header
            .chip_parts
            .iter()
            .any(|chip| !shown(dashboard, chip.title)),
        header.longest.is_some_and(|id| !shown(dashboard, id)),
        !shown(dashboard, header.title),
    ]
}

#[test]
fn the_drop_order_is_kept() {
    let (dashboard, _text) = at(1100.0, State::HiddenAndFiltering);
    let header = &dashboard.header;
    assert!(!shown(&dashboard, header.group_caption), "グループ caption");
    for chip in &header.chip_parts {
        assert!(!shown(&dashboard, chip.title), "a chip title");
        assert!(shown(&dashboard, chip.key), "a chip keycap stays");
    }
    // The title goes last, and only after the 最長 tag.
    if !shown(&dashboard, header.title) {
        let longest = header.longest.expect("the demo has a longest wait");
        assert!(!shown(&dashboard, longest), "the title went before 最長");
    }

    // Narrowing one window from 1920 to 1000 px in steps of 4: each item
    // goes only after the ones before it, none comes back, and the header
    // fits at every width.
    let (mut dashboard, mut text) = at(1920.0, State::HiddenAndFiltering);
    let mut previous = dropped(&dashboard);
    assert_eq!(previous, [false; 4], "1920 px drops nothing");
    for width in (1000..=1916).rev().step_by(4) {
        dashboard.prepare(size(width as f32), &mut text);
        let now = dropped(&dashboard);
        for (before, after) in previous.iter().zip(&now) {
            assert!(!before || *after, "{width}: an item came back: {now:?}");
        }
        for pair in now.windows(2) {
            assert!(pair[0] || !pair[1], "{width}: out of order: {now:?}");
        }
        no_overlap(&dashboard, width as f32, &format!("{width} px narrowing"));
        previous = now;
    }
    assert_eq!(previous, [true; 4], "1000 px is narrower than 1100");
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
    let (mut dashboard, mut text) = at(1100.0, State::HiddenAndFiltering);
    let caption = dashboard.header.group_caption;
    assert!(!shown(&dashboard, caption));
    dashboard.prepare(size(1920.0), &mut text);
    dashboard.prepare(size(1920.0), &mut text);
    assert!(shown(&dashboard, caption));
    for chip in &dashboard.header.chip_parts {
        assert!(shown(&dashboard, chip.title));
    }
    assert!(shown(&dashboard, dashboard.header.title));
    // And it settles: laying out again changes nothing.
    assert!(!dashboard.prepare(size(1920.0), &mut text));

    // Widening one window from 1000 to 1920 px: items come back in the
    // reverse order and none goes again, and the header fits throughout.
    let (mut dashboard, mut text) = at(1000.0, State::HiddenAndFiltering);
    let mut previous = dropped(&dashboard);
    for width in (1004..=1920).step_by(4) {
        dashboard.prepare(size(width as f32), &mut text);
        let now = dropped(&dashboard);
        for (before, after) in previous.iter().zip(&now) {
            assert!(*before || !after, "{width}: an item went again: {now:?}");
        }
        no_overlap(&dashboard, width as f32, &format!("{width} px widening"));
        previous = now;
    }
    assert_eq!(previous, [false; 4]);
}

fn count_texts(dashboard: &Dashboard) -> Vec<String> {
    labels_under(&dashboard.tree, dashboard.header.counts)
}

#[test]
fn urgency_counts_count_each_need() {
    let (dashboard, _text) = laid_out(Grouping::Orchestrator);
    assert_eq!(
        count_texts(&dashboard),
        ["承認 2", "質問 1", "失敗 1", "静止 1"]
    );

    // A fleet without failures says 失敗 0.
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let mut workers = fleet::canned();
    for worker in &mut workers.workers {
        if worker.need() == Some(crate::fleet::Need::Failure) {
            worker.state = crate::fleet::State::Finished;
            worker.attention = false;
        }
    }
    dashboard.apply(live(workers, 0), &mut text);
    assert_eq!(
        count_texts(&dashboard),
        ["承認 2", "質問 1", "失敗 0", "静止 1"]
    );
}

#[test]
fn the_counts_ignore_the_filter() {
    let (dashboard, _text) = at(1440.0, State::Filtering);
    assert_eq!(dashboard.filter.query, "perf");
    assert_eq!(
        count_texts(&dashboard),
        ["承認 2", "質問 1", "失敗 1", "静止 1"],
        "urgency is not filtered"
    );
}

#[test]
fn the_tally_tags_and_the_hint_strip_are_gone() {
    let (dashboard, _text) = laid_out(Grouping::Orchestrator);
    let tags = tags_under(&dashboard.tree, dashboard.header.bar);
    // The header's only tag is the longest wait.
    assert_eq!(tags.len(), 1, "{tags:?}");
    assert!(tags[0].starts_with("最長 "), "{tags:?}");
}
