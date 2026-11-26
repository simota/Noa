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
    key(&mut dashboard, &mut text, Key::Char('3'));
    assert_eq!(dashboard.view.scope, Scope::Current);
}

/// Each row of the list: whether it is hidden, and where it is.
fn items_on_screen(dashboard: &Dashboard) -> Vec<(bool, [f32; 4])> {
    let list = dashboard.registry.list.as_ref().unwrap();
    let tree = &dashboard.tree;
    list.items
        .iter()
        .map(|(id, _)| (tree.is_hidden(*id), rect(tree, *id)))
        .collect()
}

#[test]
fn a_wheel_page_change_reaches_the_next_frame() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    // What the window does after a frame: nothing is left to draw.
    dashboard.prepare(window(), &mut text);
    assert!(!dashboard.needs_update(window()));
    // A row hidden and not laid out since stands for a page the list does
    // not show.
    let list = dashboard.registry.list.as_ref().unwrap();
    let (card, _) = list.items[0];
    let list = list.list;
    let over_list = center(&dashboard.tree, list);
    dashboard.tree.set_hidden(card, true);
    // The wheel over the list, as the window delivers it, taken by the
    // pager (here it would turn no page: the test stands in for one).
    dashboard.pager_takes_wheel = true;
    let delta = Point::new(0.0, -40.0);
    dashboard.dispatch(
        Event::Wheel {
            pos: over_list,
            delta,
        },
        &mut text,
    );
    // The pager took it: nothing scrolled.
    let offset = dashboard
        .tree
        .get::<ScrollView>(list)
        .map(ScrollView::offset);
    assert!(offset.is_none_or(|offset| offset == 0.0), "{offset:?}");
    // The page the pager now shows is in place at once, hidden items and
    // layout both — input that comes before the next frame hits it — and
    // the next frame must still draw it.
    assert!(
        dashboard.needs_update(window()),
        "the window would keep the old page until the next input"
    );
    let at_once = items_on_screen(&dashboard);
    dashboard.prepare(window(), &mut text);
    assert_eq!(
        at_once,
        items_on_screen(&dashboard),
        "the wheel left the page to the next frame"
    );
}

#[test]
fn opening_help_cancels_a_press_underneath() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let status = notice_of;
    let before = status(&dashboard);
    // The top row waits for an approval: its button opens the pane.
    let pane = rows_of(&dashboard)[0];
    let pos = center(&dashboard.tree, row_button(&dashboard, pane));
    let button = PointerButton::Primary;
    // 開く is held down, ? opens the help, and the pointer is let go over
    // 開く: the release must not open the pane under the modal.
    dashboard.dispatch(Event::PointerMove { pos }, &mut text);
    dashboard.dispatch(Event::PointerDown { pos, button }, &mut text);
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let slash = Key::Char('/');
    dashboard.dispatch(
        Event::KeyDown {
            key: slash,
            modifiers: shift,
        },
        &mut text,
    );
    typed(&mut dashboard, &mut text, "?");
    assert!(dashboard.view.help);
    dashboard.dispatch(Event::PointerUp { pos, button }, &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(status(&dashboard), before, "開く fired under the help");
    assert_eq!(dashboard.view.selected, None);

    // Closed again, the help leaves 開く working as before.
    key(&mut dashboard, &mut text, Key::Escape);
    assert!(!dashboard.view.help);
    click(&mut dashboard, &mut text, pos);
    let label = label_of(&dashboard, pane).to_owned();
    assert_eq!(status(&dashboard), format!("{label} のペインへ移動します"));
}

/// dashboard-worker-list AC10: 1, 2 and 3 switch the list to 現在, 要対応
/// and 終了, and the scope is saved; 4 and 5 do nothing.
#[test]
fn digits_switch_the_scope_and_save_it() {
    let path = std::env::temp_dir()
        .join(format!("agent-dashboard-keys-scope-{}", std::process::id()))
        .join("view.json");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    let store = PrefsStore::at(path.clone());
    let mut dashboard = Dashboard::new(Grouping::Orchestrator).persisting(store);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let saved = || PrefsStore::at(path.clone()).load().scope;
    for (digit, scope, rows) in [
        ('2', Scope::Attention, vec![14, 15, 16, 19]),
        ('3', Scope::Ended, vec![17, 18]),
        ('1', Scope::Current, vec![14, 15, 16, 19, 11, 12, 13]),
    ] {
        key(&mut dashboard, &mut text, Key::Char(digit));
        assert_eq!(dashboard.view.scope, scope, "{digit}");
        assert_eq!(rows_of(&dashboard), rows, "{digit}");
        assert_eq!(saved(), scope, "{digit} is saved");
    }
    let before = (dashboard.view, rows_of(&dashboard));
    for digit in ['4', '5'] {
        key(&mut dashboard, &mut text, Key::Char(digit));
        assert_eq!((dashboard.view, rows_of(&dashboard)), before, "{digit}");
    }
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

/// dashboard-worker-list AC10: J / K walk the rows as shown, Enter opens
/// the selected pane, Space its drawer, `/` the filter, as before.
#[test]
fn the_list_keys_act_on_the_rows() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let rows = rows_of(&dashboard);
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(dashboard.view.selected, Some(rows[0]));
    key(&mut dashboard, &mut text, Key::Char('j'));
    key(&mut dashboard, &mut text, Key::ArrowDown);
    assert_eq!(dashboard.view.selected, Some(rows[2]));
    key(&mut dashboard, &mut text, Key::Char('k'));
    key(&mut dashboard, &mut text, Key::ArrowUp);
    assert_eq!(dashboard.view.selected, Some(rows[0]));
    key(&mut dashboard, &mut text, Key::Enter);
    let label = label_of(&dashboard, rows[0]).to_owned();
    assert_eq!(
        notice_of(&dashboard),
        format!("{label} のペインへ移動します")
    );
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(rows[0]));
    key(&mut dashboard, &mut text, Key::Escape);
    key(&mut dashboard, &mut text, Key::Char('/'));
    typed(&mut dashboard, &mut text, "/");
    assert_eq!(
        dashboard.tree.focused(),
        Some(dashboard.header.filter_field)
    );
    typed(&mut dashboard, &mut text, "2");
    assert_eq!(dashboard.filter.query, "2", "a digit types in the field");
    assert_eq!(dashboard.view.scope, Scope::Current);
}
