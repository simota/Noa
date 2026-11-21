use crate::dashboard::keymap::{BINDINGS, Mode, Route, Trigger, route};

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
