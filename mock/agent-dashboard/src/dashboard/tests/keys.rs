use crate::dashboard::keymap::{BINDINGS, Mode, Route, Trigger, route};
use noa_gpui_core::widgets::ScrollView;

use super::*;

fn event_of(trigger: Trigger, modifiers: Modifiers) -> Event {
    match trigger {
        Trigger::Key(key) => Event::KeyDown { key, modifiers },
        Trigger::Text(text) => Event::Text(text.to_owned()),
    }
}

#[test]
fn the_router_follows_the_binding_table() {
    let held = [
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        Modifiers {
            alt: true,
            ..Modifiers::default()
        },
        Modifiers {
            command: true,
            meta: true,
            ..Modifiers::on_apple()
        },
    ];
    for binding in BINDINGS {
        for plain in [Modifiers::default(), Modifiers::on_apple()] {
            let event = event_of(binding.trigger, plain);
            assert_eq!(
                route(binding.mode, false, &event),
                Route::Act(binding.action),
                "{binding:?}"
            );
        }
        if let Trigger::Key(_) = binding.trigger {
            let unbound = if binding.mode == Mode::Help {
                Route::Swallow
            } else {
                Route::Tree
            };
            for modifiers in held {
                let event = event_of(binding.trigger, modifiers);
                assert_eq!(
                    route(binding.mode, false, &event),
                    unbound,
                    "{binding:?} with {modifiers:?}"
                );
            }
        }
    }
}

#[test]
fn composition_gives_every_key_to_the_field() {
    let modifiers = Modifiers::default();
    for key in [Key::Escape, Key::Enter, Key::ArrowDown, Key::Char('1')] {
        let event = Event::KeyDown { key, modifiers };
        assert_eq!(route(Mode::Filter, true, &event), Route::Tree, "{key:?}");
    }
}

#[test]
fn help_swallows_what_it_does_not_bind() {
    let j = Event::KeyDown {
        key: Key::Char('j'),
        modifiers: Modifiers::default(),
    };
    assert_eq!(route(Mode::Help, false, &j), Route::Swallow);
    let x = Event::Text("x".into());
    assert_eq!(route(Mode::Help, false, &x), Route::Swallow);
    let pos = Point::new(10.0, 10.0);
    let press = Event::PointerDown {
        pos,
        button: PointerButton::Primary,
    };
    assert_eq!(route(Mode::Help, false, &press), Route::Tree);
}

#[test]
fn no_tree_shortcuts_are_registered() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    key(&mut dashboard, &mut text, Key::Char('/'));
    typed(&mut dashboard, &mut text, "/");
    assert_eq!(
        dashboard.tree.focused(),
        Some(dashboard.header.filter_field)
    );
    key(&mut dashboard, &mut text, Key::Char('1'));
    assert!(dashboard.panels.iter().all(Option::is_some));
}

#[test]
fn a_wheel_page_change_reaches_the_next_frame() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    // What the window does after a frame: nothing is left to draw.
    dashboard.prepare(window(), &mut text);
    assert!(!dashboard.needs_update(window()));
    // A page the pager must change: a running card it hid stands for the
    // page the wheel left behind.
    let list = dashboard.registry.lists[Panel::Running as usize]
        .as_ref()
        .unwrap();
    let (card, _) = list.items[0];
    let list = list.list;
    let over_list = center(&dashboard.tree, list);
    dashboard.tree.set_hidden(card, true);
    // The wheel over the list, as the window delivers it, taken by the
    // pager (U1's stub takes none: the test stands in for U3's).
    dashboard.pager_takes_wheel = true;
    let delta = Point::new(0.0, -40.0);
    dashboard.dispatch(
        Event::Wheel {
            pos: over_list,
            delta,
        },
        &mut text,
    );
    // The pager took it: the list (a `ScrollView` until U3) did not scroll.
    let offset = dashboard
        .tree
        .get::<ScrollView>(list)
        .map(ScrollView::offset);
    assert!(offset.is_none_or(|offset| offset == 0.0), "{offset:?}");
    assert!(
        dashboard.needs_update(window()),
        "the window would keep the old page until the next input"
    );
}
