//! AC5: the `?` help lists every binding, blocks the screen below and
//! closes again.

use noa_gpui_core::widgets::Kbd;

use super::super::keymap::{BINDINGS, Mode};
use super::*;

fn overlay(dashboard: &Dashboard) -> WidgetId {
    let mut overlays = dashboard.tree.overlays();
    let id = overlays.next().expect("help overlay");
    assert!(overlays.next().is_none(), "one overlay");
    id
}

/// One help row: the keys of each of its keycaps, in order, and its title.
type HelpRow = (Vec<Vec<String>>, String);

/// The help as built, child by child: the title, then each section's
/// heading with the rows that follow it up to the next heading, the note
/// on what a row of the list is, and the footer row last. Anything else
/// among the overlay's children fails here.
struct Help {
    title: String,
    sections: Vec<(String, Vec<HelpRow>)>,
    note: Option<String>,
    footer: WidgetId,
}

fn help_of(tree: &UiTree, root: WidgetId) -> Help {
    let label = |id: WidgetId| tree.get::<Label>(id).map(|l| l.text().to_owned());
    let kids = tree.children(root);
    let (title, rest) = kids.split_first().expect("the help has a title");
    let (footer, body) = rest.split_last().expect("the help has a footer");
    let mut sections: Vec<(String, Vec<HelpRow>)> = Vec::new();
    let mut note = None;
    for id in body {
        if let Some(text) = label(*id).filter(|text| text == help::ROW_NOTE) {
            note = Some(text);
            continue;
        }
        if let Some(heading) = label(*id) {
            sections.push((heading, Vec::new()));
            continue;
        }
        let [keys, title] = tree.children(*id) else {
            panic!("a row is a keycaps column and a title");
        };
        let caps = tree.children(*keys).iter().map(|cap| {
            let kbd = tree
                .get::<Kbd>(*cap)
                .expect("the keycaps column holds keycaps");
            kbd.keys().iter().map(|k| k.to_string()).collect()
        });
        let row = (caps.collect(), label(*title).expect("a row's title"));
        let (_, rows) = sections.last_mut().expect("a row comes under a heading");
        rows.push(row);
    }
    Help {
        title: label(*title).expect("the help's title"),
        sections,
        note,
        footer: *footer,
    }
}

/// The rows `mode`'s section must show: one per distinct title of its
/// bindings, in `BINDINGS` order, each key of the title's bindings on a
/// keycap of its own.
fn expected_rows(mode: Mode) -> Vec<HelpRow> {
    let mut rows: Vec<HelpRow> = Vec::new();
    for binding in BINDINGS.iter().filter(|b| b.mode == mode) {
        let caps = binding.keys.iter().map(|k| vec![k.to_string()]);
        match rows.iter_mut().find(|(_, title)| title == binding.title) {
            Some((keys, _)) => keys.extend(caps),
            None => rows.push((caps.collect(), binding.title.to_owned())),
        }
    }
    rows
}

#[test]
fn question_mark_opens_help_with_every_binding() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    typed(&mut dashboard, &mut text, "?");
    assert!(dashboard.view.help);
    let help = help_of(&dashboard.tree, overlay(&dashboard));
    assert_eq!(help.title, "キーボード操作");

    let want: Vec<(String, Vec<HelpRow>)> = [
        (Mode::Browse, "一覧"),
        (Mode::Filter, "絞り込み中"),
        (Mode::Help, "ヘルプ"),
    ]
    .into_iter()
    .map(|(mode, heading)| (heading.to_owned(), expected_rows(mode)))
    .collect();
    let groups: usize = want.iter().map(|(_, rows)| rows.len()).sum();
    assert!(groups >= 12, "{groups} groups");
    assert_eq!(help.sections, want);
    // dashboard-worker-list AC10: a row stands for a pane, not a task.
    assert_eq!(
        help.note.as_deref(),
        Some("1 行 = 1 ワーカーペイン（依頼の件数ではありません）")
    );
}

/// dashboard-worker-list AC10 (U5): the help names the scope keys, what
/// `state:要対応` and `need:any` take, and the tabs by their new names.
#[test]
fn the_help_names_the_scopes_the_attention_filter_and_the_tabs() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    typed(&mut dashboard, &mut text, "?");
    let help = help_of(&dashboard.tree, overlay(&dashboard));
    let (_, browse) = &help.sections[0];
    let title_of = |key: &str| {
        browse
            .iter()
            .find(|(keys, _)| keys.iter().any(|cap| cap == &[key.to_owned()]))
            .map(|(_, title)| title.as_str())
            .unwrap_or_else(|| panic!("no row for {key}"))
    };
    assert_eq!(title_of("1–3"), "表示の切替（現在・要対応・終了）");
    let filter = title_of("/");
    assert!(
        filter.contains("state:要対応 = 承認・入力・失敗"),
        "{filter}"
    );
    assert!(filter.contains("need:any = 通知・静止も含む"), "{filter}");
    assert_eq!(
        title_of("T"),
        "タブを切り替え（一覧 / 統計 / マップ / タイムライン）"
    );
    assert_eq!(title_of("G"), "グループを切り替え（タイムラインの並び）");
    // No row for 4 or 5: they are bound to nothing.
    for key in ["4", "5", "1–5"] {
        let named = browse
            .iter()
            .any(|(keys, _)| keys.iter().any(|cap| cap == &[key.to_owned()]));
        assert!(!named, "{key} has a row");
    }
}

/// AC5's footer: the note on the mouse and 閉じる side by side in one row,
/// inside the overlay.
#[test]
fn the_footer_note_and_close_share_a_row() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    typed(&mut dashboard, &mut text, "?");
    let root = overlay(&dashboard);
    let help = help_of(&dashboard.tree, root);
    let (_, close) = dashboard.registry.help.expect("the help is registered");
    let tree = &dashboard.tree;
    let kids = tree.children(help.footer);
    let note = kids
        .iter()
        .copied()
        .find(|id| tree.get::<Label>(*id).is_some())
        .expect("the footer's note");
    assert_eq!(
        tree.get::<Label>(note).unwrap().text(),
        "マウス: 行のボタン・表示の切替・分析はクリックでも操作できます"
    );
    assert!(kids.contains(&close), "閉じる is not in the footer row");
    let (note, close) = (rect(tree, note), rect(tree, close));
    let middle = |r: [f32; 4]| r[1] + r[3] / 2.0;
    assert!(
        (middle(note) - middle(close)).abs() <= 1.0,
        "{note:?} {close:?}"
    );
    assert!(note[0] + note[2] <= close[0], "{note:?} {close:?}");
    let overlay = rect(tree, root);
    assert!(
        inside(note, overlay) && inside(close, overlay),
        "{overlay:?}"
    );
}

#[test]
fn escape_and_question_mark_close_help() {
    for close in ["?", "Esc"] {
        let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
        typed(&mut dashboard, &mut text, "?");
        assert_eq!(dashboard.tree.overlays().count(), 1);
        if close == "?" {
            typed(&mut dashboard, &mut text, "?");
        } else {
            key(&mut dashboard, &mut text, Key::Escape);
        }
        assert!(!dashboard.view.help, "{close}");
        assert_eq!(dashboard.tree.overlays().count(), 0, "{close}");
    }
}

#[test]
fn help_blocks_the_keys_below() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    typed(&mut dashboard, &mut text, "?");
    let scope = dashboard.view.scope;
    let selected = dashboard.view.selected;
    for c in ['3', 'j'] {
        key(&mut dashboard, &mut text, Key::Char(c));
        typed(&mut dashboard, &mut text, &c.to_string());
    }
    assert!(dashboard.view.help);
    assert_eq!(dashboard.view.scope, scope);
    assert_eq!(dashboard.view.selected, selected);
}

#[test]
fn help_fits_the_smallest_window() {
    let size = Size::new(1100.0, 700.0);
    let mut dashboard = Dashboard::new(Grouping::Orchestrator);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(size, &mut text);
    dashboard.dispatch(Event::Text("?".to_owned()), &mut text);
    dashboard.prepare(size, &mut text);
    let r = rect(&dashboard.tree, overlay(&dashboard));
    assert!(inside(r, [0.0, 0.0, size.width, size.height]), "{r:?}");
}

/// Every label of the help (titles, headings, row descriptions, the note
/// and the footer) lies inside the overlay, at the smallest window and a
/// common one: the widths are those the layout measured with the test's
/// `TextStore`, so this holds with whatever fonts the platform has.
#[test]
fn every_help_label_lies_inside_the_overlay() {
    for size in [Size::new(1100.0, 700.0), Size::new(1440.0, 900.0)] {
        let mut dashboard = Dashboard::new(Grouping::Orchestrator);
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(size, &mut text);
        dashboard.dispatch(Event::Text("?".to_owned()), &mut text);
        dashboard.prepare(size, &mut text);
        let root = overlay(&dashboard);
        let tree = &dashboard.tree;
        let outer = rect(tree, root);
        let mut stack = vec![root];
        let mut labels = 0;
        while let Some(id) = stack.pop() {
            stack.extend(tree.children(id).iter().copied());
            if let Some(label) = tree.get::<Label>(id) {
                labels += 1;
                let r = rect(tree, id);
                assert!(
                    inside(r, outer),
                    "{size:?}: {:?} at {r:?} leaves the help at {outer:?}",
                    label.text()
                );
            }
        }
        assert!(labels >= 20, "{size:?}: {labels} labels");
    }
}
