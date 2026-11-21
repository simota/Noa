//! AC5: the `?` help lists every binding, blocks the screen below and
//! closes again.

use noa_gpui_core::widgets::Kbd;

use super::super::keymap::{BINDINGS, Mode};
use super::*;

fn text_event(dashboard: &mut Dashboard, text: &mut TextStore, s: &str) {
    dashboard.dispatch(Event::Text(s.to_owned()), text);
    dashboard.prepare(window(), text);
}

fn key_event(dashboard: &mut Dashboard, text: &mut TextStore, key: Key) {
    dashboard.dispatch(
        Event::KeyDown {
            key,
            modifiers: Modifiers::default(),
        },
        text,
    );
    dashboard.prepare(window(), text);
}

fn overlay(dashboard: &Dashboard) -> WidgetId {
    let mut overlays = dashboard.tree.overlays();
    let id = overlays.next().expect("help overlay");
    assert!(overlays.next().is_none(), "one overlay");
    id
}

/// Every row of the overlay as `(Kbd keys in order, label)`: the rows are the
/// widgets whose children are a keycaps column and the title label.
fn rows_of(tree: &UiTree, id: WidgetId) -> Vec<(Vec<String>, String)> {
    let kids = tree.children(id);
    if let [keys, title] = kids
        && let Some(label) = tree.get::<Label>(*title)
        && !tree.children(*keys).is_empty()
        && tree
            .children(*keys)
            .iter()
            .all(|k| tree.get::<Kbd>(*k).is_some())
    {
        let caps = tree
            .children(*keys)
            .iter()
            .flat_map(|k| tree.get::<Kbd>(*k).unwrap().keys().to_vec())
            .map(|k| k.to_string())
            .collect();
        return vec![(caps, label.text().to_owned())];
    }
    kids.iter().flat_map(|k| rows_of(tree, *k)).collect()
}

#[test]
fn question_mark_opens_help_with_every_binding() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    text_event(&mut dashboard, &mut text, "?");
    assert!(dashboard.view.help);
    let rows = rows_of(&dashboard.tree, overlay(&dashboard));

    let mut groups: Vec<(Mode, &str, Vec<&str>)> = Vec::new();
    for b in BINDINGS {
        match groups
            .iter_mut()
            .find(|(m, t, _)| *m == b.mode && *t == b.title)
        {
            Some((_, _, keys)) => keys.extend(b.keys),
            None => groups.push((b.mode, b.title, b.keys.to_vec())),
        }
    }
    assert!(groups.len() >= 12, "{} groups", groups.len());
    let mut at = 0;
    for (_, title, keys) in groups {
        let want: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        let found = rows[at..]
            .iter()
            .position(|(caps, label)| *label == title && *caps == want)
            .unwrap_or_else(|| panic!("no row {want:?} {title:?} in {rows:?}"));
        at += found + 1;
    }
}

#[test]
fn escape_and_question_mark_close_help() {
    for close in ["?", "Esc"] {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        text_event(&mut dashboard, &mut text, "?");
        assert_eq!(dashboard.tree.overlays().count(), 1);
        if close == "?" {
            text_event(&mut dashboard, &mut text, "?");
        } else {
            key_event(&mut dashboard, &mut text, Key::Escape);
        }
        assert!(!dashboard.view.help, "{close}");
        assert_eq!(dashboard.tree.overlays().count(), 0, "{close}");
    }
}

#[test]
fn help_blocks_the_keys_below() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    text_event(&mut dashboard, &mut text, "?");
    let shown = dashboard.view.shown;
    let selected = dashboard.view.selected;
    for c in ['1', 'j'] {
        key_event(&mut dashboard, &mut text, Key::Char(c));
        text_event(&mut dashboard, &mut text, &c.to_string());
    }
    assert!(dashboard.view.help);
    assert_eq!(dashboard.view.shown, shown);
    assert_eq!(dashboard.view.selected, selected);
}

#[test]
fn help_fits_the_smallest_window() {
    let size = Size::new(1100.0, 700.0);
    let mut dashboard = Dashboard::new(Grouping::Orchestrator);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(size, &mut text);
    text_event_at(&mut dashboard, &mut text, size);
    let r = rect(&dashboard.tree, overlay(&dashboard));
    assert!(inside(r, [0.0, 0.0, size.width, size.height]), "{r:?}");
}

fn text_event_at(dashboard: &mut Dashboard, text: &mut TextStore, size: Size) {
    dashboard.dispatch(Event::Text("?".to_owned()), text);
    dashboard.prepare(size, text);
}
