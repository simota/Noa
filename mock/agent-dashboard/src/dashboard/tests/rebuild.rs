use noa_gpui_core::Key;
use noa_gpui_core::widgets::Label;
use noa_gpui_core::{Event, Modifiers, PointerButton};
use noa_gpui_scene::Point;

use super::*;

fn layout_count(view: View) -> usize {
    let mut dashboard = Dashboard::showing(view);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    text.layouts().len()
}

fn view_of(dashboard: &Dashboard) -> View {
    dashboard.view
}

#[test]
fn hide_and_restore_cycles_release_the_old_text_layouts() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let fresh = layout_count(view_of(&dashboard));
    assert_eq!(text.layouts().len(), fresh);
    for _ in 0..3 {
        press(&mut dashboard, &mut text, '1', Modifiers::default());
        press(&mut dashboard, &mut text, '1', Modifiers::default());
    }
    assert_eq!(text.layouts().len(), fresh, "after hide/restore cycles");
    // A screen with a panel off owns fewer layouts than one with all on.
    press(&mut dashboard, &mut text, '2', Modifiers::default());
    assert_eq!(text.layouts().len(), layout_count(view_of(&dashboard)));
    assert!(text.layouts().len() < fresh);
}

#[test]
fn a_grouping_change_releases_the_old_text_layouts() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let control = dashboard.grouping_control;
    let origin = dashboard.tree.window_origin(control).unwrap();
    let rect = dashboard.tree.layout(control).unwrap().rect;
    let pos = Point::new(origin.x + rect.width - 12.0, origin.y + rect.height / 2.0);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.grouping, Grouping::Flat);
    assert_eq!(text.layouts().len(), layout_count(view_of(&dashboard)));
}

#[test]
fn a_details_click_releases_the_old_text_layouts() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let (button, label) = dashboard.queue_buttons.details[1];
    let pos = center(&dashboard.tree, button);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.expanded, Some(label));
    assert_eq!(text.layouts().len(), layout_count(view_of(&dashboard)));
}

fn last_segment(dashboard: &Dashboard) -> Point {
    let control = dashboard.grouping_control;
    let origin = dashboard.tree.window_origin(control).unwrap();
    let rect = dashboard.tree.layout(control).unwrap().rect;
    Point::new(origin.x + rect.width - 12.0, origin.y + rect.height / 2.0)
}

#[test]
fn a_press_right_after_a_panel_toggle_hits_the_grouping_control() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let pos = last_segment(&dashboard);
    let button = PointerButton::Primary;
    let key = Key::Char('2');
    let modifiers = Modifiers::default();
    // The window delivers input before it lays out for the next frame:
    // no `prepare` between the toggle and the press.
    dashboard.dispatch(Event::KeyDown { key, modifiers }, &mut text);
    assert!(dashboard.panels[Panel::Running as usize].is_none());
    dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
    dashboard.prepare(window(), &mut text);
    dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
    assert_eq!(dashboard.view.grouping, Grouping::Flat);
}

#[test]
fn a_press_right_after_a_details_click_hits_the_open_button() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let (details, label) = dashboard.queue_buttons.details[1];
    let pos = center(&dashboard.tree, details);
    let button = PointerButton::Primary;
    dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
    dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
    assert_eq!(dashboard.view.expanded, Some(label));
    // The new 開く button's place is known from the rebuilt tree before any
    // `prepare`, so the press can be aimed at it.
    let (open, _) = dashboard.queue_buttons.open[0];
    let pos = center(&dashboard.tree, open);
    dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
    dashboard.prepare(window(), &mut text);
    dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
    let status = dashboard.tree.get::<Label>(dashboard.status).unwrap();
    let label = label_of(&dashboard, label);
    assert!(status.text().contains(label), "{}", status.text());
}

/// What the window does after input and on the next frame: it redraws only if
/// `needs_update`, and presents only if `prepare` says the list changed. The
/// list it presents must be the rebuilt screen's, the same as a screen built
/// for that view from scratch.
fn assert_next_frame_shows_the_rebuild(
    dashboard: &mut Dashboard,
    text: &mut TextStore,
    before: &[String],
) {
    assert!(
        dashboard.needs_update(window()),
        "no redraw is requested after the rebuild"
    );
    assert!(
        dashboard.prepare(window(), text),
        "the frame after the rebuild presents nothing"
    );
    let shown = frame_of(dashboard.display_list(), text);
    let fresh = Dashboard::showing(dashboard.view);
    assert_eq!(shown, fresh_frame(fresh));
    assert_ne!(shown, before, "the frame still shows the old screen");
}

#[test]
fn a_panel_toggle_reaches_the_next_frame() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let before = frame_of(dashboard.display_list(), &text);
    let key = Key::Char('2');
    let modifiers = Modifiers::default();
    dashboard.dispatch(Event::KeyDown { key, modifiers }, &mut text);
    assert!(dashboard.panels[Panel::Running as usize].is_none());
    assert_next_frame_shows_the_rebuild(&mut dashboard, &mut text, &before);
}

#[test]
fn a_grouping_change_reaches_the_next_frame() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let before = frame_of(dashboard.display_list(), &text);
    let pos = last_segment(&dashboard);
    let button = PointerButton::Primary;
    dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
    dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
    assert_eq!(dashboard.view.grouping, Grouping::Flat);
    assert_next_frame_shows_the_rebuild(&mut dashboard, &mut text, &before);
}

#[test]
fn a_details_click_reaches_the_next_frame() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let before = frame_of(dashboard.display_list(), &text);
    let (details, label) = dashboard.queue_buttons.details[1];
    let pos = center(&dashboard.tree, details);
    let button = PointerButton::Primary;
    dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
    dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
    assert_eq!(dashboard.view.expanded, Some(label));
    assert_next_frame_shows_the_rebuild(&mut dashboard, &mut text, &before);
}

#[test]
fn a_rebuild_keeps_the_appearance() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    dashboard.set_system_appearance(Appearance::Light);
    assert_eq!(dashboard.tree.theme().appearance, Appearance::Light);
    press(&mut dashboard, &mut text, '1', Modifiers::default());
    assert!(dashboard.panels[Panel::Queue as usize].is_none());
    assert_eq!(dashboard.tree.theme().appearance, Appearance::Light);
}
