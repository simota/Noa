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
            for text in long {
                let (id, _, shown) = cut
                    .iter()
                    .find(|(_, full, _)| *full == text)
                    .unwrap_or_else(|| panic!("{text} is a tag: {cut:?}"));
                let whole = text.len() < 20;
                let start = shown.trim_end_matches('\u{2026}');
                assert!(
                    if whole {
                        *shown == text
                    } else {
                        shown.ends_with('\u{2026}') && text.starts_with(start) && !start.is_empty()
                    },
                    "{size:?} pane {pane}: {text:?} shown as {shown:?}"
                );
                // The tag draws that text, inside itself.
                let (drawn, _) = drawn_text(&dashboard, *id);
                let area = rect(tree, *id);
                assert!(
                    inside(drawn, area),
                    "{size:?} pane {pane}: {shown:?} drawn at {drawn:?} outside {area:?}"
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
