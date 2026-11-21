use noa_gpui_core::widget::Widget;
use noa_gpui_core::widgets::{Container, Label};
use noa_gpui_core::{Role, accesskit};
use noa_gpui_scene::Color;
use noa_gpui_text::TextStyle;

use super::*;

/// Presses J until `pane` is selected.
fn select(dashboard: &mut Dashboard, text: &mut TextStore, pane: u64) {
    select_at(dashboard, text, pane, window());
}

/// A key pressed with no modifier, then the next frame's layout at `size`.
fn key_at(dashboard: &mut Dashboard, text: &mut TextStore, key: Key, size: Size) {
    let modifiers = Modifiers::default();
    dashboard.dispatch(Event::KeyDown { key, modifiers }, text);
    dashboard.prepare(size, text);
}

/// [`select`] in a window of `size`.
fn select_at(dashboard: &mut Dashboard, text: &mut TextStore, pane: u64, size: Size) {
    for _ in 0..20 {
        if dashboard.view.selected == Some(pane) {
            return;
        }
        key_at(dashboard, text, Key::Char('j'), size);
    }
    panic!("J never selects {pane}");
}

/// The drawer's root, if it is open.
fn drawer(dashboard: &Dashboard) -> Option<WidgetId> {
    dashboard.registry.drawer.map(|(root, _)| root)
}

/// The widgets under `id` (itself included) that are drawn: a hidden
/// widget, or one inside a hidden widget, is left out.
fn shown_under(tree: &UiTree, id: WidgetId) -> Vec<WidgetId> {
    let mut found = Vec::new();
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        if tree.is_hidden(id) {
            continue;
        }
        found.push(id);
        stack.extend(tree.children(id).iter().rev());
    }
    found
}

/// What a label or tag says (a cut tag: as drawn).
fn text_of(tree: &UiTree, id: WidgetId) -> Option<String> {
    tree.get::<Label>(id)
        .map(|label| label.text().to_owned())
        .or_else(|| tree.get::<Tag>(id).map(|tag| tag.text().to_owned()))
        .or_else(|| {
            tree.get::<drawer::FitTag>(id)
                .map(|tag| tag.shown_text().to_owned())
        })
}

/// The labels and tags the drawer shows: those hidden, or inside a hidden
/// row, are not seen and not listed.
fn drawer_texts(dashboard: &Dashboard) -> Vec<String> {
    let root = drawer(dashboard).expect("the drawer is open");
    let tree = &dashboard.tree;
    shown_under(tree, root)
        .into_iter()
        .filter_map(|id| text_of(tree, id))
        .collect()
}

/// The shown label in the drawer that says `wanted`.
fn drawer_label(dashboard: &Dashboard, wanted: &str) -> WidgetId {
    let root = drawer(dashboard).expect("the drawer is open");
    let tree = &dashboard.tree;
    shown_under(tree, root)
        .into_iter()
        .find(|id| tree.get::<Label>(*id).is_some_and(|l| l.text() == wanted))
        .unwrap_or_else(|| panic!("no shown label {wanted:?} in the drawer"))
}

/// Where the text of `label` is drawn (`[x, y, w, h]`, the text layout's
/// box) and in which colour.
fn drawn_text(dashboard: &Dashboard, label: WidgetId) -> ([f32; 4], Color) {
    let area = rect(&dashboard.tree, label);
    dashboard
        .display_list()
        .commands()
        .iter()
        .rev()
        .find_map(|command| match command {
            DrawCommand::Text { rect: r, color, .. } if inside([r.x, r.y, 0.0, 0.0], area) => {
                Some(([r.x, r.y, r.width, r.height], *color))
            }
            _ => None,
        })
        .expect("the label draws its text")
}

/// What the text drawn inside `id` says: the string of the text layout
/// that the last `DrawCommand::Text` inside it draws (the one
/// [`drawn_text`] finds).
fn drawn_string(dashboard: &Dashboard, text: &TextStore, id: WidgetId) -> String {
    let area = rect(&dashboard.tree, id);
    let layout = dashboard
        .display_list()
        .commands()
        .iter()
        .rev()
        .find_map(|command| match command {
            DrawCommand::Text {
                rect: r, layout, ..
            } if inside([r.x, r.y, 0.0, 0.0], area) => Some(*layout),
            _ => None,
        })
        .expect("the widget draws text");
    let buffer = text.layouts().buffer(layout);
    let lines: Vec<&str> = buffer.lines.iter().map(|line| line.text()).collect();
    lines.join("\n")
}

/// How wide `line` is on one line in `style`.
fn width_in(text: &mut TextStore, line: &str, style: TextStyle) -> f32 {
    let id = text.create(line, style, None);
    let width = text.size(id).width;
    text.remove(id);
    width
}

fn shows(dashboard: &Dashboard, wanted: &str) -> bool {
    drawer_texts(dashboard).iter().any(|t| t == wanted)
}

/// A live dashboard showing `fleet`.
fn live_of(fleet: Fleet) -> (Dashboard, TextStore) {
    live_at(fleet, window())
}

/// A live dashboard showing `fleet` in a window of `size`.
fn live_at(fleet: Fleet, size: Size) -> (Dashboard, TextStore) {
    let (channel, snapshots, _notices, _commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(size, &mut text);
    snapshots.send(live(fleet, 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(size, &mut text);
    (dashboard, text)
}

#[test]
fn space_opens_the_drawer_for_any_worker() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 17);
    assert_eq!(dashboard.tree.overlays().count(), 0);
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.tree.overlays().count(), 1);
    assert_eq!(dashboard.view.detail, Some(17));
    for wanted in [
        "lint-pass",
        "依頼",
        "clippy の警告をすべて解消する",
        "所要 16 分",
        "結果あり",
    ] {
        assert!(
            shows(&dashboard, wanted),
            "{wanted}: {:?}",
            drawer_texts(&dashboard)
        );
    }
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.tree.overlays().count(), 0);
    assert_eq!(dashboard.view.detail, None);
    assert!(drawer(&dashboard).is_none());
}

#[test]
fn the_card_details_button_opens_the_drawer() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let (button, _) = *dashboard
        .registry
        .detail_buttons
        .iter()
        .find(|(_, pane)| *pane == 11)
        .expect("running card 11 has a 詳細 button");
    let pos = center(&dashboard.tree, button);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.detail, Some(11));
    assert_eq!(dashboard.view.selected, Some(11));
    assert!(drawer(&dashboard).is_some());
}

#[test]
fn the_drawer_follows_the_selection() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 11);
    key(&mut dashboard, &mut text, Key::Space);
    assert!(shows(&dashboard, label_of(&dashboard, 11)));
    key(&mut dashboard, &mut text, Key::Char('j'));
    assert_eq!(dashboard.view.selected, Some(12));
    assert_eq!(dashboard.view.detail, Some(12));
    assert_eq!(dashboard.tree.overlays().count(), 1);
    assert!(shows(&dashboard, label_of(&dashboard, 12)));
    assert!(!shows(&dashboard, label_of(&dashboard, 11)));
}

/// The drawer's lines of screen, when it shows a tail: the shown labels
/// inside the box that holds them (the queue card's box:
/// `Role::Background`).
fn tail_lines(dashboard: &Dashboard) -> Vec<WidgetId> {
    let root = drawer(dashboard).expect("the drawer is open");
    let tree = &dashboard.tree;
    let background = tree.theme().color(Role::Background);
    let labels = |id| -> Vec<WidgetId> {
        shown_under(tree, id)
            .into_iter()
            .filter(|id| tree.get::<Label>(*id).is_some())
            .collect()
    };
    shown_under(tree, root)
        .into_iter()
        .filter(|id| tree.get::<Label>(*id).is_none())
        .find(|id| {
            !labels(*id).is_empty()
                && quad_at(dashboard, rect(tree, *id)).is_some_and(|(color, _)| color == background)
        })
        .map(labels)
        .unwrap_or_default()
}

fn tail_texts(dashboard: &Dashboard) -> Vec<String> {
    tail_lines(dashboard)
        .into_iter()
        .filter_map(|id| text_of(&dashboard.tree, id))
        .collect()
}

/// An empty tail is said in the section captions' style: 11 px, muted.
fn assert_caption(dashboard: &Dashboard, text: &mut TextStore, wanted: &str) {
    let (drawn, color) = drawn_text(dashboard, drawer_label(dashboard, wanted));
    assert_eq!(color, dashboard.tree.theme().color(Role::Muted), "{wanted}");
    let caption = width_in(text, wanted, TextStyle::new(11.0));
    let body = width_in(text, wanted, TextStyle::new(12.0));
    assert!(
        (drawn[2] - caption).abs() <= 0.5 && (body - caption).abs() > 1.0,
        "{wanted}: drawn {} wide, 11 px {caption}, 12 px {body}",
        drawn[2]
    );
}

/// Line `index` of the drawer's tail, which says `line`, is drawn in the
/// 11 px monospace font: as wide as `line` is in it. Returns that width.
fn assert_monospace(dashboard: &Dashboard, text: &mut TextStore, index: usize, line: &str) -> f32 {
    let (drawn, _) = drawn_text(dashboard, tail_lines(dashboard)[index]);
    let monospace = TextStyle::new(11.0).family(text.font_family("monospace"));
    let mono = width_in(text, line, monospace);
    assert!(
        (drawn[2] - mono).abs() <= 0.5,
        "{line}: drawn {} wide, {mono} in monospace",
        drawn[2]
    );
    mono
}

#[test]
fn the_drawer_shows_the_screen_tail() {
    let mut fleet = fleet::canned();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 12)
        .unwrap()
        .screen_tail = vec!["$ make".to_owned()];
    let (mut dashboard, mut text) = live_of(fleet);
    select(&mut dashboard, &mut text, 12);
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(tail_texts(&dashboard), ["$ make"]);
    assert!(!shows(&dashboard, "まだ取得していません"));
    assert_monospace(&dashboard, &mut text, 0, "$ make");

    // A line whose width tells the fonts apart well: "$ make" is about as
    // wide in either.
    let narrow = "iiiiiiiiii";
    let mut fleet = fleet::canned();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 12)
        .unwrap()
        .screen_tail = vec!["$ make".to_owned(), narrow.to_owned()];
    let (mut dashboard, mut text) = live_of(fleet);
    select(&mut dashboard, &mut text, 12);
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(tail_texts(&dashboard), ["$ make", narrow]);
    let proportional = width_in(&mut text, narrow, TextStyle::new(11.0));
    let mono = assert_monospace(&dashboard, &mut text, 1, narrow);
    assert!(mono - proportional > 10.0, "{mono} vs {proportional}");

    let (mut dashboard, mut text) = live_of(fleet::canned());
    select(&mut dashboard, &mut text, 12);
    key(&mut dashboard, &mut text, Key::Space);
    assert!(tail_lines(&dashboard).is_empty());
    assert_caption(&dashboard, &mut text, "まだ取得していません");

    // The demo has nothing to fetch.
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 12);
    key(&mut dashboard, &mut text, Key::Space);
    assert_caption(&dashboard, &mut text, "なし");
}

#[test]
fn a_rebuild_keeps_the_drawer_open() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    select(&mut dashboard, &mut text, 11);
    key(&mut dashboard, &mut text, Key::Space);
    key(&mut dashboard, &mut text, Key::Char('5'));
    assert!(!dashboard.view.shown[Panel::Graph as usize]);
    assert_eq!(dashboard.view.detail, Some(11));
    assert_eq!(dashboard.tree.overlays().count(), 1);
    assert!(shows(&dashboard, label_of(&dashboard, 11)));
}

#[test]
fn the_time_rows_follow_the_worker_in_place() {
    let (mut dashboard, mut text) = live_of(fleet::canned());
    select(&mut dashboard, &mut text, 11);
    key(&mut dashboard, &mut text, Key::Space);
    assert!(shows(&dashboard, "人待ち合計 3 分"));
    let mut fleet = dashboard.feed.fleet.clone();
    let worker = fleet.workers.iter_mut().find(|w| w.pane == 11).unwrap();
    worker.phases.push((fleet::Phase::Input, 5.0));
    worker.started_minutes = Some(30.0);
    worker.since = "1 分前に出力".to_owned();
    let before = drawer(&dashboard);
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(drawer(&dashboard), before, "no rebuild");
    for wanted in [
        "人待ち合計 8 分",
        "開始 30 分前",
        "1 分前に出力",
        "入力待ち 5 分",
    ] {
        assert!(
            shows(&dashboard, wanted),
            "{wanted}: {:?}",
            drawer_texts(&dashboard)
        );
    }
}

#[test]
fn the_drawer_stays_inside_the_window() {
    for (width, height) in [(1100.0, 700.0), (1440.0, 900.0), (1920.0, 1200.0)] {
        let size = Size::new(width, height);
        let mut dashboard = Dashboard::new(Grouping::Orchestrator);
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(size, &mut text);
        for pane in [13, 11, 17] {
            select(&mut dashboard, &mut text, pane);
            dashboard.dispatch(
                Event::KeyDown {
                    key: Key::Space,
                    modifiers: Modifiers::default(),
                },
                &mut text,
            );
            dashboard.prepare(size, &mut text);
            let root = drawer(&dashboard).unwrap();
            let drawer = rect(&dashboard.tree, root);
            assert!(
                inside(drawer, [0.0, 0.0, width, height]),
                "{drawer:?} in {width}x{height}"
            );
            let (open, close) = (
                dashboard.registry.open_buttons.last().unwrap().0,
                dashboard.registry.drawer.unwrap().1,
            );
            for button in [open, close] {
                assert!(inside(rect(&dashboard.tree, button), drawer));
            }
            dashboard.dispatch(
                Event::KeyDown {
                    key: Key::Space,
                    modifiers: Modifiers::default(),
                },
                &mut text,
            );
        }
    }
}

/// `GroupBox::card` made the running cards 247 / 231 / 255 px and the done
/// cards 198 px tall (measured before U1). The caption line (15 px) became a
/// row with a small 詳細 button, 26.32 px tall: every card grew by 11.32 px.
///
/// Measured where every card is shown whole: a hidden card (one a list
/// pages out) measures 0. The cards' widths are fixed (336 px running, the
/// 270 px done panel), so their heights do not depend on the window; at
/// 1440×1800 the lists are about 900 px taller than at 1440×900, far more
/// than both panels' cards need, so nothing has to page out.
#[test]
fn the_running_cards_keep_their_height() {
    const ROW_GROWTH: f32 = 26.32 - 15.0;
    let size = Size::new(1440.0, 1800.0);
    let mut dashboard = Dashboard::new(Grouping::Orchestrator);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(size, &mut text);
    let tree = &dashboard.tree;
    for (panel, pane, group_box) in [
        (Panel::Running, 11, 247.0),
        (Panel::Running, 12, 231.0),
        (Panel::Running, 13, 255.0),
        (Panel::Done, 17, 198.0),
        (Panel::Done, 18, 198.0),
    ] {
        let parts = dashboard.registry.lists[panel as usize].as_ref().unwrap();
        let (card, _) = *parts.items.iter().find(|(_, p)| *p == Some(pane)).unwrap();
        assert!(!tree.is_hidden(card), "card {pane} is shown");
        let card_rect = rect(tree, card);
        assert!(
            inside(card_rect, rect(tree, parts.list)),
            "card {pane} {card_rect:?} lies inside its list"
        );
        let height = card_rect[3];
        assert!(
            (height - (group_box + ROW_GROWTH)).abs() <= 0.5,
            "card {pane}: {height} vs {}",
            group_box + ROW_GROWTH
        );
    }
}

/// AC8: the start row says "開始 {started}前", or "不明" when the start is
/// not known.
#[test]
fn an_unknown_start_says_unknown() {
    let mut fleet = fleet::canned();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 11)
        .unwrap()
        .started_minutes = None;
    let (mut dashboard, mut text) = live_of(fleet);
    select(&mut dashboard, &mut text, 11);
    key(&mut dashboard, &mut text, Key::Space);
    let started = pulse_id(&dashboard, |p| matches!(p, Pulse::Drawer(11)));
    assert_eq!(dashboard.tree.get::<Label>(started).unwrap().text(), "不明");
    assert!(!shows(&dashboard, "開始 不明"));

    let mut fleet = dashboard.feed.fleet.clone();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 11)
        .unwrap()
        .started_minutes = Some(30.0);
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(
        dashboard.tree.get::<Label>(started).unwrap().text(),
        "開始 30 分前"
    );
    let mut fleet = dashboard.feed.fleet.clone();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 11)
        .unwrap()
        .started_minutes = None;
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(dashboard.tree.get::<Label>(started).unwrap().text(), "不明");
}

/// A long command name as the kind, a long branch, a long model: every tag
/// stays inside the drawer's content, cut with an ellipsis, and the
/// worker's label is not cut for them. A long label is cut too, but keeps
/// most of the head row: the kind gives way first.
#[test]
fn long_tags_stay_inside_the_drawer() {
    let branch = "W".repeat(80);
    let command = "cargo-nextest-run-workspace-all-features-no-fail-fast-retries-3";
    let model = "gemini-4-argon-experimental-long-context-preview-2026-11-01";
    for size in [Size::new(1100.0, 700.0), Size::new(1440.0, 900.0)] {
        let mut fleet = fleet::canned();
        for worker in &mut fleet.workers {
            match worker.pane {
                // A plain command: kind, branch, no model.
                12 => {
                    worker.kind = command.to_owned();
                    worker.branch.clone_from(&branch);
                }
                // An agent: kind, model, branch.
                11 => {
                    worker.model = model.to_owned();
                    worker.branch.clone_from(&branch);
                }
                // A long label and a long kind.
                13 => {
                    worker.label = LONG_LABEL.to_owned();
                    worker.kind = command.to_owned();
                }
                _ => {}
            }
        }
        let (mut dashboard, mut text) = live_at(fleet, size);
        // In the order J visits them.
        for pane in [13, 11, 12] {
            select_at(&mut dashboard, &mut text, pane, size);
            if dashboard.view.detail != Some(pane) {
                key_at(&mut dashboard, &mut text, Key::Space, size);
            }
            assert_eq!(dashboard.view.detail, Some(pane));
            let root = drawer(&dashboard).unwrap();
            let tree = &dashboard.tree;
            let content = tree.children(root)[0];
            let inner = rect(tree, content);
            let rows = &tree.children(content)[..2];
            for row in rows {
                for id in shown_under(tree, *row) {
                    let r = rect(tree, id);
                    assert!(
                        inside(r, inner),
                        "{size:?} pane {pane}: {:?} {r:?} outside {inner:?}",
                        text_of(tree, id)
                    );
                }
            }
            // Each long text is cut with an ellipsis, short ones are whole.
            let cut: Vec<(WidgetId, String, String)> = rows
                .iter()
                .flat_map(|row| shown_under(tree, *row))
                .filter_map(|id| {
                    let tag = tree.get::<drawer::FitTag>(id)?;
                    Some((id, tag.text().to_owned(), tag.shown_text().to_owned()))
                })
                .collect();
            let worker = dashboard.feed.fleet.worker(pane).unwrap();
            let mut long = vec![worker.kind.clone(), worker.branch.clone()];
            if worker.is_agent() {
                long.push(worker.model_line());
            }
            for full in long {
                let (id, _, shown) = cut
                    .iter()
                    .find(|(_, given, _)| *given == full)
                    .unwrap_or_else(|| panic!("{full} is a tag: {cut:?}"));
                let whole = full.len() < 20;
                let start = shown.trim_end_matches('\u{2026}');
                assert!(
                    if whole {
                        *shown == full
                    } else {
                        shown.ends_with('\u{2026}') && full.starts_with(start) && !start.is_empty()
                    },
                    "{size:?} pane {pane}: {full:?} shown as {shown:?}"
                );
                // The tag draws that text, inside itself.
                let (drawn, _) = drawn_text(&dashboard, *id);
                let area = rect(tree, *id);
                assert!(
                    inside(drawn, area),
                    "{size:?} pane {pane}: {shown:?} drawn at {drawn:?} outside {area:?}"
                );
                assert_eq!(
                    drawn_string(&dashboard, &text, *id),
                    *shown,
                    "{size:?} pane {pane}: what the tag draws"
                );
            }
            let label = label_of(&dashboard, pane);
            let head = shown_under(tree, rows[0]);
            let label_id = *head
                .iter()
                .find(|id| tree.get::<Label>(**id).is_some_and(|l| l.text() == label))
                .expect("the head shows the label");
            let shown = tree.get::<Label>(label_id).unwrap().shown_text();
            if label == LONG_LABEL {
                let width = rect(tree, label_id)[2];
                assert!(
                    width >= inner[2] / 2.0,
                    "{size:?}: the long label keeps {width} of {}",
                    inner[2]
                );
            } else {
                assert_eq!(shown, label, "{size:?}: the label is not cut");
            }
        }
    }
}

/// D1: a model name with line breaks (live input) is drawn on one line,
/// inside its tag; its accessibility label keeps the text as it came.
#[test]
fn a_tag_with_line_breaks_is_drawn_on_one_line() {
    let model = "a\nb\r\nc";
    let mut fleet = fleet::canned();
    let worker = fleet.workers.iter_mut().find(|w| w.pane == 11).unwrap();
    worker.model = model.to_owned();
    worker.effort.clear();
    let (mut dashboard, mut text) = live_of(fleet);
    select(&mut dashboard, &mut text, 11);
    key(&mut dashboard, &mut text, Key::Space);
    let tree = &dashboard.tree;
    let id = shown_under(tree, drawer(&dashboard).unwrap())
        .into_iter()
        .find(|id| {
            tree.get::<drawer::FitTag>(*id)
                .is_some_and(|tag| tag.text() == model)
        })
        .expect("the model is a tag");
    let tag = tree.get::<drawer::FitTag>(id).unwrap();
    assert_eq!(tag.shown_text(), "a b c");
    let (drawn, _) = drawn_text(&dashboard, id);
    let area = rect(tree, id);
    assert!(inside(drawn, area), "drawn at {drawn:?} outside {area:?}");
    let mut node = accesskit::Node::new(accesskit::Role::Label);
    tag.accessibility(&mut node);
    assert_eq!(node.label(), Some(model));
}

/// A [`drawer::FitTag`] saying `says`, alone in a column `width` wide that
/// stretches it to that width, laid out and drawn.
fn fit_tag_in(text: &mut TextStore, says: &str, width: f32) -> (UiTree, WidgetId) {
    let mut tree = UiTree::new();
    let column = tree.insert(None, Container::column().width(width));
    let tag = tree.insert(Some(column), drawer::FitTag::new(says));
    tree.prepare(Size::new(400.0, 100.0), text);
    (tree, tag)
}

/// The rects of the quads and texts `tree` draws.
fn drawn_rects(tree: &UiTree) -> Vec<[f32; 4]> {
    tree.display_list()
        .commands()
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Quad { rect: r, .. } | DrawCommand::Text { rect: r, .. } => {
                Some([r.x, r.y, r.width, r.height])
            }
            _ => None,
        })
        .collect()
}

/// D2: a tag given less room than an ellipsis needs draws nothing; at any
/// width, what it draws lies inside the room it is given.
#[test]
fn a_tag_draws_nothing_outside_its_room() {
    let mut text = TextStore::new(Default::default());
    let (tree, tag) = fit_tag_in(&mut text, "main", 0.0);
    assert_eq!(tree.get::<drawer::FitTag>(tag).unwrap().shown_text(), "");
    assert_eq!(drawn_rects(&tree), Vec::<[f32; 4]>::new(), "nothing drawn");
    for width in 0..=80 {
        let (tree, tag) = fit_tag_in(&mut text, "main", width as f32);
        let area = rect(&tree, tag);
        for drawn in drawn_rects(&tree) {
            assert!(
                inside(drawn, area),
                "{width}: {:?} drawn at {drawn:?} outside {area:?}",
                tree.get::<drawer::FitTag>(tag).unwrap().shown_text()
            );
        }
    }
    let (tree, tag) = fit_tag_in(&mut text, "main", 80.0);
    assert_eq!(
        tree.get::<drawer::FitTag>(tag).unwrap().shown_text(),
        "main"
    );
}

/// D3: a cut keeps whole grapheme clusters: a keycap emoji (1, U+FE0F,
/// U+20E3) is kept whole or left out, never cut down to its "1".
#[test]
fn a_cut_keeps_whole_grapheme_clusters() {
    let keycap = "1\u{FE0F}\u{20E3}";
    let rest = "c".repeat(20);
    let full = format!("ab{keycap}{rest}");
    // Where a start of `full` may end: before or after a, b, the whole
    // keycap and each c.
    let mut ends = vec![0, 1, 2];
    ends.extend((0..=rest.len()).map(|n| 2 + keycap.len() + n));
    let mut text = TextStore::new(Default::default());
    let mut seen = Vec::new();
    for width in 0..=200 {
        let (tree, tag) = fit_tag_in(&mut text, &full, width as f32);
        let shown = tree.get::<drawer::FitTag>(tag).unwrap().shown_text();
        let start = shown.strip_suffix('\u{2026}').unwrap_or(shown);
        assert!(
            full.starts_with(start) && ends.contains(&start.len()),
            "{width}: {shown:?}"
        );
        seen.push(start.to_owned());
    }
    // The widths tried cut both before and after the keycap.
    for start in ["ab".to_owned(), format!("ab{keycap}")] {
        assert!(seen.contains(&start), "{start:?} in {seen:?}");
    }
}

/// Texts from live data that end a bidi paragraph inside one line of text
/// with left-to-right text before and right-to-left text after: U+2029,
/// NEL U+0085 and U+001C. The shaper splits a text into lines at CR and LF
/// only, and panics on a line that holds paragraphs of both directions.
/// The last is the review's model name: a label splits it at the LF, but
/// the tag used to turn the LF into a space first.
const PARAGRAPH_BREAKS: [&str; 4] = [
    "A\u{2029}\u{5D0}",
    "A\u{85}\u{5D0}",
    "A\u{1C}\u{5D0}",
    "A\u{2029}\n\u{5D0}",
];

/// Whether `text` holds a character [`drawer::one_line`] turns into a
/// space.
fn breaks_lines(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

/// A live dashboard, grouped by `grouping`, on the canned fleet with
/// worker 11 (a running agent) changed by `edit`, laid out with 11's card
/// shown and its drawer open. The timeline and the graph are hidden: they
/// draw worker labels themselves, outside the drawer and the cards (see
/// the fix report).
fn hostile(grouping: Grouping, edit: impl Fn(&mut Worker)) -> (Dashboard, TextStore) {
    let mut fleet = fleet::canned();
    edit(fleet.workers.iter_mut().find(|w| w.pane == 11).unwrap());
    let (channel, snapshots, _notices, _commands) = Channel::detached();
    let mut dashboard = Dashboard::live(grouping, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    for digit in ['4', '5'] {
        press(&mut dashboard, &mut text, digit, Modifiers::default());
    }
    assert_eq!(
        dashboard.view.shown,
        shown_only(&[Panel::Timeline, Panel::Graph])
    );
    snapshots.send(live(fleet, 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    select(&mut dashboard, &mut text, 11);
    key(&mut dashboard, &mut text, Key::Space);
    assert_eq!(dashboard.view.detail, Some(11));
    (dashboard, text)
}

/// Every label and tag shown in the drawer and in the running and done
/// lists is drawn on one line; returns what they say.
fn assert_one_line_everywhere(dashboard: &Dashboard, what: &str) -> Vec<String> {
    let tree = &dashboard.tree;
    let mut roots = vec![drawer(dashboard).expect("the drawer is open")];
    for panel in [Panel::Running, Panel::Done] {
        roots.extend(
            dashboard.registry.lists[panel as usize]
                .as_ref()
                .map(|l| l.list),
        );
    }
    let texts: Vec<String> = roots
        .into_iter()
        .flat_map(|root| shown_under(tree, root))
        .filter_map(|id| text_of(tree, id))
        .collect();
    for shown in &texts {
        assert!(!breaks_lines(shown), "{what}: {shown:?} is drawn");
    }
    texts
}

/// D1: a kind, model or branch with a paragraph break (live input) is
/// drawn on one line in the drawer's tags, the card's tags and the group
/// titles; the drawer's tags keep the text as given for assistive
/// technology. Before the fix, laying it out panicked in the shaper.
#[test]
fn tags_with_a_paragraph_break_are_drawn_on_one_line() {
    for given in PARAGRAPH_BREAKS {
        // A kind that is not an agent's has no model tag: the kind is
        // tried with the branch, grouped by agent (the kind is a group
        // title), and the model with the branch, grouped by model.
        for grouping in [Grouping::Agent, Grouping::Model] {
            let (dashboard, _text) = hostile(grouping, |worker| {
                if matches!(grouping, Grouping::Agent) {
                    worker.kind = given.to_owned();
                } else {
                    worker.model = given.to_owned();
                    worker.effort.clear();
                }
                worker.branch = given.to_owned();
            });
            let what = format!("{given:?} {grouping:?}");
            let texts = assert_one_line_everywhere(&dashboard, &what);
            let line = drawer::one_line(given);
            // The drawer's two tags, the card's two tags, the group title.
            let shown = texts.iter().filter(|t| t.starts_with(&line)).count();
            assert_eq!(shown, 5, "{what}: {line:?} in {texts:?}");
            let tree = &dashboard.tree;
            let tags: Vec<&drawer::FitTag> = shown_under(tree, drawer(&dashboard).unwrap())
                .into_iter()
                .filter_map(|id| tree.get::<drawer::FitTag>(id))
                .filter(|tag| tag.text() == given)
                .collect();
            assert_eq!(tags.len(), 2, "{what}: two tags in the drawer");
            for tag in tags {
                let mut node = accesskit::Node::new(accesskit::Role::Label);
                tag.accessibility(&mut node);
                assert_eq!(node.label(), Some(given), "{what}");
            }
        }
    }
}

/// D1: a cut tag's tooltip, which is drawn, is its one-line text.
#[test]
fn a_cut_tag_shows_its_one_line_text_as_its_tooltip() {
    let mut text = TextStore::new(Default::default());
    for given in PARAGRAPH_BREAKS {
        let long = format!("{}{given}", "W".repeat(40));
        let (tree, id) = fit_tag_in(&mut text, &long, 100.0);
        let tag = tree.get::<drawer::FitTag>(id).unwrap();
        assert_ne!(tag.shown_text(), drawer::one_line(&long), "cut");
        assert_eq!(
            tag.tooltip(Point::new(0.0, 0.0)),
            Some(drawer::one_line(&long))
        );
    }
}

/// D1: a worker label with a paragraph break is drawn on one line as the
/// drawer's head and as the card's title.
#[test]
fn a_label_with_a_paragraph_break_is_drawn_on_one_line() {
    for given in PARAGRAPH_BREAKS {
        let (dashboard, _text) = hostile(Grouping::Orchestrator, |worker| {
            worker.label = given.to_owned();
        });
        let texts = assert_one_line_everywhere(&dashboard, &format!("{given:?}"));
        let line = drawer::one_line(given);
        let shown = texts.iter().filter(|t| **t == line).count();
        assert_eq!(
            shown, 2,
            "{given:?}: the head and the card title in {texts:?}"
        );
    }
}

/// D1: a task and a detail with a paragraph break are drawn as one
/// paragraph, wrapped, in the drawer and on the card.
#[test]
fn a_task_and_detail_with_a_paragraph_break_are_drawn_as_one_paragraph() {
    for given in PARAGRAPH_BREAKS {
        let (dashboard, _text) = hostile(Grouping::Orchestrator, |worker| {
            worker.task = given.to_owned();
            worker.detail = given.to_owned();
        });
        let texts = assert_one_line_everywhere(&dashboard, &format!("{given:?}"));
        let line = drawer::one_line(given);
        for wanted in [line.clone(), format!("依頼: {line}")] {
            assert!(
                texts.contains(&wanted),
                "{given:?}: {wanted:?} in {texts:?}"
            );
        }
    }
}

/// D1: a screen tail line with a paragraph break is drawn on one line.
#[test]
fn a_screen_tail_line_with_a_paragraph_break_is_drawn_on_one_line() {
    for given in PARAGRAPH_BREAKS {
        let (dashboard, _text) = hostile(Grouping::Orchestrator, |worker| {
            worker.screen_tail = vec![given.to_owned(), "$ make".to_owned()];
        });
        assert_one_line_everywhere(&dashboard, &format!("{given:?}"));
        assert_eq!(
            tail_texts(&dashboard),
            [drawer::one_line(given), "$ make".to_owned()]
        );
    }
}

#[test]
fn one_line_turns_breaks_and_controls_into_spaces() {
    assert_eq!(
        drawer::one_line("a\nb\r\nc\rd\te\u{85}f\u{1C}g\u{2028}h\u{2029}i\u{0}j"),
        "a b c d e f g h i j"
    );
    // Bidi isolates and marks are kept.
    let isolated = "\u{2067}\u{5D0}\u{2069} \u{200F}";
    assert_eq!(drawer::one_line(isolated), isolated);
}

/// Where each grapheme cluster of `line` ends, found as the drawer found
/// them before D2: an editor's caret moved to the end of each character.
/// Right, but in time square in a cluster's length.
fn ends_by_moving_the_caret(line: &str) -> Vec<usize> {
    let mut editor = noa_gpui_core::text_edit::TextEditor::single_line();
    editor.set_text(line);
    let mut ends: Vec<usize> = line
        .char_indices()
        .map(|(i, c)| {
            editor.move_to(i + c.len_utf8(), false);
            editor.caret()
        })
        .filter(|end| *end > 0)
        .collect();
    ends.dedup();
    ends
}

/// D2: cluster ends are found in time proportional to the line: for a
/// letter with 50 000 combining marks, and for 5 000 flags (regional
/// indicator pairs), the editor is handed at most a few bytes per byte of
/// the line. Moving one caret along the line, as before, hands it the
/// whole line once per character: 50 001 × 100 003 bytes.
#[test]
fn cluster_ends_take_time_in_proportion_to_the_line() {
    let marks = format!("a{}b", "\u{301}".repeat(50_000));
    let mut scanned = 0;
    let ends = drawer::cluster_ends(&marks, &mut scanned);
    assert_eq!(ends, [marks.len() - 1, marks.len()]);
    assert!(
        scanned <= 4 * marks.len(),
        "{scanned} bytes for {}",
        marks.len()
    );

    let flags = "\u{1F1EF}\u{1F1F5}".repeat(5_000);
    let mut scanned = 0;
    let ends = drawer::cluster_ends(&flags, &mut scanned);
    assert_eq!(ends, (1..=5_000).map(|n| 8 * n).collect::<Vec<_>>());
    assert!(
        scanned <= 8 * flags.len(),
        "{scanned} bytes for {}",
        flags.len()
    );
}

/// D2: the ends found piece by piece are the ends the editor finds over
/// the whole line, also for clusters longer than a piece and for
/// sequences whose rules look back (emoji, flags, an odd run of regional
/// indicators, an Indic conjunct, a prepended mark).
#[test]
fn cluster_ends_are_the_editors() {
    let unit = format!(
        "x{} e\u{301} \u{1F44D}\u{1F3FD} \u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F467} \
         a{}\u{1F1EF} \u{915}\u{94D}\u{937}\u{93F} \u{600} 1\u{FE0F}\u{20E3}{}end",
        "\u{301}".repeat(40),
        "\u{1F1EF}\u{1F1F5}".repeat(16),
        "\u{1F469}\u{200D}".repeat(12),
    );
    for line in [unit.clone(), unit.repeat(3), format!("y{unit}")] {
        assert_eq!(
            drawer::cluster_ends(&line, &mut 0),
            ends_by_moving_the_caret(&line),
            "{line:?}"
        );
    }
    assert_eq!(drawer::cluster_ends("", &mut 0), Vec::<usize>::new());
}

/// D3: U+0600 and the space after it are one grapheme cluster: a cut keeps
/// both or neither, never U+0600 without its space.
#[test]
fn a_cut_keeps_a_cluster_with_a_space_whole() {
    let full = format!("a\u{600} {}", "W".repeat(20));
    // Where a start of `full` may end: before or after a, the cluster of
    // U+0600 and the space, and each W.
    let mut ends = vec![0, 1];
    ends.extend((0..=20).map(|n| 4 + n));
    let mut text = TextStore::new(Default::default());
    let mut seen = Vec::new();
    for width in 0..=300 {
        let (tree, tag) = fit_tag_in(&mut text, &full, width as f32);
        let shown = tree.get::<drawer::FitTag>(tag).unwrap().shown_text();
        let start = shown.strip_suffix('\u{2026}').unwrap_or(shown);
        assert!(
            full.starts_with(start) && ends.contains(&start.len()),
            "{width}: {shown:?}"
        );
        seen.push(start.to_owned());
    }
    assert!(seen.iter().any(|s| s == "a\u{600} "), "{seen:?}");
}
