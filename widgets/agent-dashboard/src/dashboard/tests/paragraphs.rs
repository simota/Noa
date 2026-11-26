//! A text with a paragraph separator and both text directions
//! ("A\u{2029}\u{5D0}") makes cosmic-text's shaper panic. The live feed puts
//! every text from Noa on one line (`live.rs`); these tests hold the
//! dashboard to it for a fleet that did not come through the feed (as the
//! canned one does not) and for what is typed or pasted into the filter.

use noa_gpui_core::widgets::TextField;

use crate::fleet::one_line;

use super::*;

const BROKEN: &str = "A\u{2029}\u{5D0}";

/// Whether `text` would be shaped as more than one paragraph.
fn breaks(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

/// `text` behind `BROKEN`: a text that holds it, and stays told apart.
fn broken(text: &str) -> String {
    format!("{BROKEN}{text}")
}

/// The canned fleet with `BROKEN` in every text of every worker and
/// orchestrator, built directly as no live snapshot could be.
///
/// But for `since`, unless `raw_since`: the running and done cards'
/// captions (`cards.rs`) and the drawer's time rows (`drawer.rs`) draw it as
/// it is, and those files are not this unit's to change. That one text goes
/// through `one_line`, as `Worker::on_one_line` (the live feed's
/// constructor) puts it.
fn broken_fleet(raw_since: bool) -> Fleet {
    let mut fleet = fleet::canned();
    for w in &mut fleet.workers {
        for text in [
            &mut w.label,
            &mut w.kind,
            &mut w.branch,
            &mut w.model,
            &mut w.effort,
            &mut w.orchestrator,
            &mut w.task,
            &mut w.detail,
            &mut w.since,
        ] {
            *text = broken(text);
        }
        if !raw_since {
            w.since = one_line(&w.since);
        }
        w.screen_tail = vec![broken("$ make"), broken("ok")];
    }
    for o in &mut fleet.orchestrators {
        o.name = broken(&o.name);
    }
    fleet
}

/// A broken fleet as the live feed would hand it on: every worker through
/// `Worker::on_one_line`, every orchestrator's name on one line.
fn constructed(mut fleet: Fleet) -> Fleet {
    fleet.workers = fleet.workers.into_iter().map(Worker::on_one_line).collect();
    for o in &mut fleet.orchestrators {
        o.name = one_line(&o.name);
    }
    fleet
}

/// Every label and tag text the screen holds, the overlays' included.
fn texts(dashboard: &Dashboard) -> Vec<String> {
    let tree = &dashboard.tree;
    tree.root()
        .into_iter()
        .chain(tree.overlays())
        .flat_map(|root| {
            labels_under(tree, root)
                .into_iter()
                .chain(tags_under(tree, root))
        })
        .collect()
}

#[test]
fn a_fleet_with_paragraph_breaks_draws_in_the_list_and_the_drawer() {
    let sizes = [
        Size::new(1100.0, 700.0),
        Size::new(1440.0, 900.0),
        Size::new(1920.0, 1200.0),
    ];
    // The live feed's constructor makes any fleet safe: one size shows it.
    let fleets = [
        ("raw", broken_fleet(false), &sizes[..]),
        ("constructed", constructed(broken_fleet(true)), &sizes[1..2]),
    ];
    for (how, fleet, sizes) in fleets {
        for grouping in Grouping::ALL {
            for &size in sizes {
                let view = View {
                    // docs-sync (running) in the drawer.
                    selected: Some(12),
                    detail: Some(12),
                    ..View::of(grouping)
                };
                let feed = Feed {
                    fleet: fleet.clone(),
                    ..Feed::default()
                };
                let mut dashboard = Dashboard::start(view, feed);
                let mut text = TextStore::new(Default::default());
                dashboard.prepare(size, &mut text);
                let what = format!("{how} fleet, {grouping:?} at {size:?}");
                assert!(dashboard.registry.list.is_some(), "{what}");
                assert_eq!(rows_of(&dashboard).len(), 7, "{what}: 現在's rows");
                assert!(dashboard.registry.drawer.is_some(), "{what}: drawer");
                // What time changes is drawn again in place.
                dashboard.refresh(&mut text);
                dashboard.prepare(size, &mut text);
                assert!(!frame_of(dashboard.display_list(), &text).is_empty());

                let texts = texts(&dashboard);
                let shown: Vec<&String> = texts.iter().filter(|t| t.contains('\u{5D0}')).collect();
                assert!(shown.len() > 20, "{what}: {} texts", shown.len());
                for text in &texts {
                    assert!(!breaks(text), "{what}: {text:?}");
                }
            }
        }
    }
}

/// The filter field's text.
fn field_text(dashboard: &Dashboard) -> String {
    let field = dashboard.header.filter_field;
    dashboard
        .tree
        .get::<TextField>(field)
        .unwrap()
        .text()
        .to_owned()
}

fn paste(dashboard: &mut Dashboard, text: &mut TextStore, pasted: &str) {
    dashboard.tree.clipboard().write_text(pasted);
    let modifiers = Modifiers {
        meta: true,
        command: true,
        command_is_meta: true,
        ..Modifiers::default()
    };
    let key = Key::Char('v');
    dashboard.dispatch(Event::KeyDown { key, modifiers }, text);
    dashboard.prepare(window(), text);
}

fn count_text(dashboard: &Dashboard) -> String {
    let count = dashboard.header.match_count;
    dashboard
        .tree
        .get::<Label>(count)
        .unwrap()
        .text()
        .to_owned()
}

#[test]
fn a_paragraph_break_pasted_into_the_filter_separates_terms() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    typed(&mut dashboard, &mut text, "/");
    paste(&mut dashboard, &mut text, "codex\u{2029}perf");
    assert_eq!(field_text(&dashboard), "codex perf");
    assert_eq!(dashboard.filter.query, "codex perf");
    assert_eq!(dashboard.filter.caret, "codex perf".len());
    let field = dashboard.header.filter_field;
    assert_eq!(dashboard.tree.get::<TextField>(field).unwrap().caret(), 10);
    // perf-probe is the one codex worker whose texts hold "perf".
    assert_eq!(count_text(&dashboard), "1 / 9 件");
    assert_eq!(dashboard.order(), [13]);

    // Right-to-left text after the break: drawn, and it matches no one.
    key(&mut dashboard, &mut text, Key::Escape);
    typed(&mut dashboard, &mut text, "/");
    paste(&mut dashboard, &mut text, BROKEN);
    assert_eq!(field_text(&dashboard), "A \u{5D0}");
    assert_eq!(count_text(&dashboard), "0 / 9 件");
    assert!(!frame_of(dashboard.display_list(), &text).is_empty());

    // Pasted in the middle, the caret ends after what was pasted.
    key(&mut dashboard, &mut text, Key::Escape);
    typed(&mut dashboard, &mut text, "/");
    typed(&mut dashboard, &mut text, "ab");
    key(&mut dashboard, &mut text, Key::ArrowLeft);
    paste(&mut dashboard, &mut text, "\u{2028}\u{5D0}\u{2029}");
    assert_eq!(field_text(&dashboard), "a \u{5D0} b");
    let caret = "a \u{5D0} ".len();
    assert_eq!(
        dashboard.tree.get::<TextField>(field).unwrap().caret(),
        caret
    );
    assert_eq!(dashboard.filter.caret, caret);
}

#[test]
fn typed_or_composed_paragraph_breaks_reach_the_filter_on_one_line() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    typed(&mut dashboard, &mut text, "/");
    // Text an input method commits.
    typed(&mut dashboard, &mut text, BROKEN);
    assert_eq!(field_text(&dashboard), "A \u{5D0}");
    assert_eq!(dashboard.filter.query, "A \u{5D0}");

    // A composition shows in the field before it is committed: its cursor
    // moves with the text it is in.
    let composition = Event::Composition {
        text: format!("\u{5D0}{BROKEN}"),
        cursor: Some((3, 6)),
    };
    assert_eq!(
        filter::one_line_input(composition.clone()),
        Event::Composition {
            text: "\u{5D0}A \u{5D0}".to_owned(),
            cursor: Some((3, 4)),
        }
    );
    dashboard.dispatch(composition, &mut text);
    dashboard.prepare(window(), &mut text);
    assert!(dashboard.composing());
    let field = dashboard.header.filter_field;
    let shown = dashboard
        .tree
        .get::<TextField>(field)
        .unwrap()
        .composition();
    assert!(!breaks(shown), "{shown:?}");
    assert!(!frame_of(dashboard.display_list(), &text).is_empty());
    assert_eq!(one_line(BROKEN), "A \u{5D0}");
}
