use crate::dashboard::panel::{DONE_WIDTH, QUEUE_WIDTH};
use crate::dashboard::style::HEADER_HEIGHT;
use noa_gpui_core::{Event, Modifiers, PointerButton};
use noa_gpui_scene::{Point, Size};

use super::*;

#[test]
fn every_card_needing_you_shows_without_scrolling() {
    for grouping in Grouping::ALL {
        let (dashboard, _text) = laid_out(grouping);
        let tree = &dashboard.tree;
        let buttons = &dashboard.queue_buttons;
        // The quiet 13 is folded away under 参考情報.
        assert_eq!(buttons.open.len() + buttons.details.len(), 4);
        for (button, label) in buttons.open.iter().chain(&buttons.details) {
            let card = tree.parent(tree.parent(*button).unwrap()).unwrap();
            let list = tree.parent(card).unwrap();
            assert!(
                bottom(tree, card) <= bottom(tree, list),
                "{label} ends at {} below the list at {} ({grouping:?})",
                bottom(tree, card),
                bottom(tree, list)
            );
        }
    }
}

#[test]
fn the_last_segment_removes_the_groups() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let control = dashboard.grouping_control;
    let origin = dashboard.tree.window_origin(control).unwrap();
    let rect = dashboard.tree.layout(control).unwrap().rect;
    let pos = Point::new(origin.x + rect.width - 12.0, origin.y + rect.height / 2.0);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.grouping, Grouping::Flat);
}

fn assert_row_spans(dashboard: &Dashboard, members: &[Panel]) {
    let rects: Vec<[f32; 4]> = members
        .iter()
        .filter_map(|p| dashboard.panels[*p as usize])
        .map(|id| rect(&dashboard.tree, id))
        .collect();
    let (first, last) = (rects.first().unwrap(), rects.last().unwrap());
    assert!((first[0] - GUTTER).abs() <= 0.5, "{rects:?}");
    assert!(
        (last[0] + last[2] - (window().width - GUTTER)).abs() <= 0.5,
        "{rects:?}"
    );
    for pair in rects.windows(2) {
        let gap = pair[1][0] - (pair[0][0] + pair[0][2]);
        assert!((gap - GUTTER).abs() <= 0.5, "{rects:?}");
    }
}

#[test]
fn all_panels_on_keeps_todays_layout() {
    let expected = [
        [12.0, 60.0, 400.0, 536.0],
        [424.0, 60.0, 722.0, 536.0],
        [1158.0, 60.0, 270.0, 536.0],
        [12.0, 608.0, 974.0, 256.0],
        [998.0, 608.0, 430.0, 256.0],
    ];
    for grouping in Grouping::ALL {
        let (dashboard, _text) = showing(grouping, [true; 5]);
        for panel in Panel::ALL {
            let id = dashboard.panels[panel as usize].unwrap();
            let actual = rect(&dashboard.tree, id);
            assert_rect(actual, expected[panel as usize], &format!("{panel:?}"));
        }
    }
}

#[test]
fn a_hidden_panel_leaves_its_row_to_the_others() {
    for hidden in Panel::ALL {
        let (dashboard, _text) = showing(Grouping::Orchestrator, shown_only(&[hidden]));
        assert!(dashboard.panels[hidden as usize].is_none());
        assert_row_spans(&dashboard, &Panel::NOW);
        assert_row_spans(&dashboard, &Panel::HISTORY);
    }
    // Without its grower a row splits its free width equally over the
    // remaining panels, on top of each one's fixed width: the 1416 px row
    // less 12 px gap less 670 px fixed leaves 734 px, 367 px each.
    let row = window().width - 2.0 * GUTTER;
    let width_of = |dashboard: &Dashboard, panel: Panel| {
        rect(&dashboard.tree, dashboard.panels[panel as usize].unwrap())[2]
    };
    let (dashboard, _text) = showing(Grouping::Orchestrator, shown_only(&[Panel::Running]));
    let free = row - GUTTER - QUEUE_WIDTH - DONE_WIDTH;
    for (panel, fixed) in [(Panel::Queue, QUEUE_WIDTH), (Panel::Done, DONE_WIDTH)] {
        let width = width_of(&dashboard, panel);
        assert!(
            (width - (fixed + free / 2.0)).abs() <= 0.5,
            "{panel:?} is {width} px with running off"
        );
    }
    let (dashboard, _text) = showing(Grouping::Orchestrator, shown_only(&[Panel::Timeline]));
    let width = width_of(&dashboard, Panel::Graph);
    assert!(
        (width - row).abs() <= 0.5,
        "graph is {width} px with timeline off"
    );
    let body_height = window().height - HEADER_HEIGHT - NOTICE_HEIGHT - 2.0 * GUTTER;
    let (dashboard, _text) = showing(Grouping::Orchestrator, shown_only(&Panel::NOW));
    assert_row_spans(&dashboard, &Panel::HISTORY);
    let id = dashboard.panels[Panel::Timeline as usize].unwrap();
    assert_eq!(rect(&dashboard.tree, id)[3].round(), body_height.round());
    let (dashboard, _text) = showing(Grouping::Orchestrator, shown_only(&Panel::HISTORY));
    assert_row_spans(&dashboard, &Panel::NOW);
    let id = dashboard.panels[Panel::Queue as usize].unwrap();
    assert_eq!(rect(&dashboard.tree, id)[3].round(), body_height.round());
}

#[test]
fn the_queue_fits_with_any_other_panels_hidden() {
    let others = [Panel::Running, Panel::Done, Panel::Timeline, Panel::Graph];
    for grouping in Grouping::ALL {
        for mask in 0..16u8 {
            let hidden: Vec<Panel> = others
                .iter()
                .enumerate()
                .filter(|(i, _)| mask >> i & 1 == 1)
                .map(|(_, p)| *p)
                .collect();
            let (dashboard, _text) = showing(grouping, shown_only(&hidden));
            let tree = &dashboard.tree;
            let buttons = &dashboard.queue_buttons;
            // The quiet 13 is folded away under 参考情報.
            assert_eq!(buttons.open.len() + buttons.details.len(), 4);
            for (button, label) in buttons.open.iter().chain(&buttons.details) {
                let card = tree.parent(tree.parent(*button).unwrap()).unwrap();
                let list = tree.parent(card).unwrap();
                assert!(
                    bottom(tree, card) <= bottom(tree, list),
                    "{label} overflows with {hidden:?} hidden ({grouping:?})"
                );
            }
        }
    }
}

#[test]
fn each_shortcut_hides_and_restores_its_panel() {
    let expected = [
        [12.0, 60.0, 400.0, 536.0],
        [424.0, 60.0, 722.0, 536.0],
        [1158.0, 60.0, 270.0, 536.0],
        [12.0, 608.0, 974.0, 256.0],
        [998.0, 608.0, 430.0, 256.0],
    ];
    for modifiers in [Modifiers::default(), Modifiers::on_apple()] {
        for (digit, panel, _) in KEYS {
            let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
            press(&mut dashboard, &mut text, digit, modifiers);
            for other in Panel::ALL {
                let present = dashboard.panels[other as usize].is_some();
                assert_eq!(present, other != panel, "{other:?} after hiding {panel:?}");
            }
            press(&mut dashboard, &mut text, digit, modifiers);
            let id = dashboard.panels[panel as usize].unwrap();
            let actual = rect(&dashboard.tree, id);
            assert_rect(actual, expected[panel as usize], &format!("{panel:?}"));
        }
    }
}

#[test]
fn the_grouping_control_stays_reachable_at_1140() {
    let size = Size::new(1140.0, 900.0);
    let mut dashboard = Dashboard::new(Grouping::Orchestrator);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(size, &mut text);
    let control = dashboard.grouping_control;
    let origin = dashboard.tree.window_origin(control).unwrap();
    let width = dashboard.tree.layout(control).unwrap().rect.width;
    let right = origin.x + width;
    assert!(right <= size.width - 16.0 + 0.5, "control ends at {right}");
    let pos = Point::new(right - 12.0, origin.y + 8.0);
    let button = PointerButton::Primary;
    dashboard.dispatch(Event::PointerMove { pos }, &mut text);
    dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
    dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
    assert_eq!(dashboard.view.grouping, Grouping::Flat);
}

fn bottom(tree: &UiTree, id: WidgetId) -> f32 {
    tree.window_origin(id).unwrap().y + tree.layout(id).unwrap().rect.height
}
