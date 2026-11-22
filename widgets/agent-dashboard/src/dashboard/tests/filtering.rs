use std::collections::BTreeSet;

use std::time::Duration;

use crate::dashboard::filter::matches;
use noa_gpui_core::StyleSheet;
use noa_gpui_core::clipboard::{Clipboard, MemoryClipboard};
use noa_gpui_core::widgets::{Badge, Label, TextField};

use super::*;

fn matching(query: &str) -> BTreeSet<u64> {
    fleet::canned()
        .workers
        .iter()
        .filter(|w| matches(w, query))
        .map(|w| w.pane)
        .collect()
}

#[test]
fn the_filter_matches_label_agent_model_branch_and_task() {
    let set = |panes: &[u64]| panes.iter().copied().collect::<BTreeSet<u64>>();
    assert_eq!(matching("codex"), set(&[13, 15, 19]), "agent");
    assert_eq!(matching("perf"), set(&[13]), "label and branch");
    assert_eq!(matching("テスト"), set(&[11, 14]), "task");
    assert_eq!(matching("claude sonnet"), set(&[14, 18]), "agent and model");
    assert_eq!(matching("claude　sonnet"), set(&[14, 18]), "U+3000");
    assert_eq!(matching("CODEX"), matching("codex"));
    assert_eq!(matching("").len(), 9);
    // Each field on its own: every query is found in that field of its
    // workers and in no other field of any worker.
    assert_eq!(matching("probe"), set(&[13]), "label perf-probe");
    assert_eq!(matching("agy"), set(&[12, 17]), "agent");
    assert_eq!(matching("gpt"), set(&[13, 15, 19]), "model");
    assert_eq!(matching("config-v2"), set(&[15]), "branch feat/config-v2");
    assert_eq!(matching("シナリオ"), set(&[19]), "task e2e シナリオを…");
}

/// `/` as the platform sends it: the key, then the text it types.
fn slash(dashboard: &mut Dashboard, text: &mut TextStore) {
    key(dashboard, text, Key::Char('/'));
    typed(dashboard, text, "/");
}

fn field(dashboard: &Dashboard) -> &TextField {
    dashboard
        .tree
        .get::<TextField>(dashboard.header.filter_field)
        .unwrap()
}

fn panes_in(dashboard: &Dashboard, panel: Panel) -> Vec<u64> {
    let list = dashboard.registry.lists[panel as usize].as_ref().unwrap();
    list.items.iter().filter_map(|(_, pane)| *pane).collect()
}

fn count_label(dashboard: &Dashboard) -> Option<String> {
    let id = dashboard.header.match_count;
    let tree = &dashboard.tree;
    (!tree.is_hidden(id)).then(|| tree.get::<Label>(id).unwrap().text().to_owned())
}

#[test]
fn the_filter_applies_to_all_five_panels() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    slash(&mut dashboard, &mut text);
    typed(&mut dashboard, &mut text, "codex");
    // 判断待ち, then 要確認; the quiet 13 is folded away under 参考情報.
    assert_eq!(panes_in(&dashboard, Panel::Queue), [15, 19]);
    assert_eq!(panes_in(&dashboard, Panel::Running), [13]);
    assert!(panes_in(&dashboard, Panel::Done).is_empty());
    let tree = &dashboard.tree;
    let done = dashboard.panels[Panel::Done as usize].unwrap();
    assert!(labels_under(tree, done).iter().any(|l| l == "該当なし"));
    assert_eq!(visible_items(&dashboard, Panel::Timeline).len(), 3);
    let graph = labels_under(tree, dashboard.panels[Panel::Graph as usize].unwrap());
    for name in ["perf-wish", "perf-probe", "e2e-smoke", "migrate-cfg"] {
        assert!(graph.iter().any(|l| l == name), "{name} in {graph:?}");
    }
    assert!(!graph.iter().any(|l| l == "release-0.9.7"), "{graph:?}");
    assert_eq!(count_label(&dashboard).as_deref(), Some("3 / 9 件"));
}

/// The count badge in a panel's title row.
fn badge_of(dashboard: &Dashboard, panel: Panel) -> Option<String> {
    let tree = &dashboard.tree;
    let row = tree.children(dashboard.panels[panel as usize].unwrap())[0];
    let badge = tree
        .children(row)
        .iter()
        .find_map(|id| tree.get::<Badge>(*id));
    badge.and_then(Badge::label)
}

#[test]
fn the_timeline_counts_its_lanes() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    assert_eq!(badge_of(&dashboard, Panel::Timeline).as_deref(), Some("9"));
    slash(&mut dashboard, &mut text);
    typed(&mut dashboard, &mut text, "codex");
    assert_eq!(badge_of(&dashboard, Panel::Timeline).as_deref(), Some("3"));
    assert_eq!(visible_items(&dashboard, Panel::Timeline).len(), 3);
}

#[test]
fn a_query_of_blanks_shows_the_count() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    slash(&mut dashboard, &mut text);
    typed(&mut dashboard, &mut text, "\u{3000}");
    assert_eq!(dashboard.filter.query, "\u{3000}");
    assert_eq!(count_label(&dashboard).as_deref(), Some("9 / 9 件"));
    assert_eq!(panes_in(&dashboard, Panel::Timeline).len(), 9);
}

#[test]
fn slash_focuses_the_field_without_typing_a_slash() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    slash(&mut dashboard, &mut text);
    assert_eq!(
        dashboard.tree.focused(),
        Some(dashboard.header.filter_field)
    );
    assert_eq!(field(&dashboard).text(), "");
}

#[test]
fn digits_type_into_the_filter_without_toggling_panels() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    slash(&mut dashboard, &mut text);
    key(&mut dashboard, &mut text, Key::Char('1'));
    typed(&mut dashboard, &mut text, "1");
    assert!(dashboard.panels.iter().all(Option::is_some));
    assert_eq!(dashboard.filter.query, "1");
    assert_eq!(field(&dashboard).text(), "1");
}

#[test]
fn escape_clears_the_filter_and_enter_keeps_it() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    slash(&mut dashboard, &mut text);
    typed(&mut dashboard, &mut text, "perf");
    key(&mut dashboard, &mut text, Key::Enter);
    assert_eq!(dashboard.tree.focused(), None);
    assert_eq!(dashboard.filter.query, "perf");
    assert_eq!(count_label(&dashboard).as_deref(), Some("1 / 9 件"));
    // Only the quiet 13 matches: folded away, and the queue says how many
    // wait on a decision outside the filter.
    assert!(panes_in(&dashboard, Panel::Queue).is_empty());
    assert!(dashboard.queue_buttons.info_toggle.is_some());
    let tree = &dashboard.tree;
    let queue = dashboard.panels[Panel::Queue as usize].unwrap();
    let hidden = queue::hidden_text(3);
    assert!(labels_under(tree, queue).contains(&hidden));

    key(&mut dashboard, &mut text, Key::Escape);
    assert_eq!(dashboard.filter.query, "");
    assert_eq!(field(&dashboard).text(), "");
    assert_eq!(count_label(&dashboard), None);
    assert_eq!(panes_in(&dashboard, Panel::Queue).len(), 4);
    assert_eq!(panes_in(&dashboard, Panel::Running).len(), 3);
    assert_eq!(panes_in(&dashboard, Panel::Done).len(), 2);
    assert_eq!(panes_in(&dashboard, Panel::Timeline).len(), 9);
}

/// Marks the header: a rebuild makes the status label again with the
/// link state, so the mark is gone after one. (Widget ids do not show a
/// rebuild: a new tree hands out the same ids again.)
fn mark_header(dashboard: &mut Dashboard) {
    let status = dashboard.status;
    dashboard
        .tree
        .update::<Label, _>(status, |l, cx| l.set_text(cx, "mark"));
}

fn header_marked(dashboard: &Dashboard) -> bool {
    dashboard
        .tree
        .get::<Label>(dashboard.status)
        .unwrap()
        .text()
        == "mark"
}

#[test]
fn typing_keeps_focus_caret_and_header() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let id = dashboard.header.filter_field;
    slash(&mut dashboard, &mut text);
    mark_header(&mut dashboard);
    typed(&mut dashboard, &mut text, "c");
    typed(&mut dashboard, &mut text, "o");
    assert!(header_marked(&dashboard), "the header was rebuilt");
    assert_eq!(panes_in(&dashboard, Panel::Queue), [15, 19], "co");
    assert_eq!(dashboard.header.filter_field, id);
    assert_eq!(dashboard.tree.focused(), Some(id));
    assert_eq!(field(&dashboard).caret(), 2);
}

#[test]
fn a_full_rebuild_keeps_the_query_and_focus() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    slash(&mut dashboard, &mut text);
    typed(&mut dashboard, &mut text, "co");
    let control = dashboard.grouping_control;
    let origin = dashboard.tree.window_origin(control).unwrap();
    let size = dashboard.tree.layout(control).unwrap().rect;
    let pos = Point::new(origin.x + size.width - 12.0, origin.y + size.height / 2.0);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.grouping, Grouping::Flat);
    let new = dashboard.header.filter_field;
    assert_eq!(field(&dashboard).text(), "co");
    assert_eq!(dashboard.tree.focused(), Some(new));
    assert_eq!(field(&dashboard).caret(), 2);
}

#[test]
fn a_snapshot_waits_for_the_composition() {
    let (channel, snapshots, _notices, _commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    slash(&mut dashboard, &mut text);
    let id = dashboard.header.filter_field;
    let composing = Event::Composition {
        text: "けん".into(),
        cursor: None,
    };
    dashboard.dispatch(composing, &mut text);
    assert!(dashboard.composing());
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(dashboard.header.filter_field, id);
    assert!(dashboard.composing(), "rebuilt while composing");
    assert!(dashboard.feed.fleet.workers.is_empty());

    typed(&mut dashboard, &mut text, "検");
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(dashboard.feed.fleet.workers.len(), 9, "the snapshot waits");
    assert_eq!(field(&dashboard).text(), "検");
}

#[test]
fn a_held_snapshot_keeps_its_age() {
    let (channel, snapshots, _notices, _commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    slash(&mut dashboard, &mut text);
    let composing = Event::Composition {
        text: "けん".into(),
        cursor: None,
    };
    dashboard.dispatch(composing, &mut text);
    // Synced as it was sent; the composition holds it for 3 s.
    let came = Instant::now();
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.take_snapshots(came, &mut text);
    assert!(dashboard.feed.fleet.workers.is_empty(), "held");
    typed(&mut dashboard, &mut text, "検");
    let applied = came + Duration::from_secs(3);
    dashboard.take_snapshots(applied, &mut text);
    assert_eq!(dashboard.feed.fleet.workers.len(), 9, "applied");
    assert_eq!(dashboard.feed.synced_at, Some(came));
    let status = crate::dashboard::health::status(&dashboard.feed, applied);
    assert_eq!(status, "ワーカー 9 · 同期 3 秒前");
}

#[test]
fn a_full_rebuild_keeps_what_the_app_installed_on_the_tree() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    // What the app installs at start: the system clipboard (holding what
    // another app copied), a stylesheet, the OS click interval, and input
    // recording when asked for.
    let mut clipboard = MemoryClipboard::default();
    clipboard.write_text("codex");
    let sheet = StyleSheet::parse("#nothing-here { background-color: red; }").unwrap();
    let tree = dashboard.tree_mut();
    tree.set_clipboard(Box::new(clipboard));
    tree.set_stylesheet(sheet);
    tree.set_multi_click(0.7, 6.0);
    tree.start_recording();

    // A full rebuild: a panel toggle.
    key(&mut dashboard, &mut text, Key::Char('5'));
    assert!(dashboard.panels[Panel::Graph as usize].is_none());

    slash(&mut dashboard, &mut text);
    let command = Modifiers {
        command: true,
        meta: true,
        ..Modifiers::on_apple()
    };
    let paste = Event::KeyDown {
        key: Key::Char('v'),
        modifiers: command,
    };
    dashboard.dispatch(paste, &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(dashboard.filter.query, "codex", "⌘V pastes the copied text");
    assert_eq!(count_label(&dashboard).as_deref(), Some("3 / 9 件"));
    let tree = dashboard.tree();
    let sheet = format!("{:?}", tree.stylesheet());
    assert!(sheet.contains("#nothing-here"), "the stylesheet: {sheet}");
    assert_eq!(tree.multi_click(), (0.7, 6.0));
    assert!(tree.is_recording());
}

/// A live fleet of running workers only (api-refactor, docs-sync,
/// perf-probe): nothing is done.
fn running_only() -> Snapshot {
    let mut fleet = fleet::canned();
    fleet.workers.retain(|w| w.state == fleet::State::Running);
    live(fleet, 0)
}

#[test]
fn a_query_of_blanks_says_which_panels_have_nobody() {
    let snapshot = running_only();
    let running = snapshot.fleet.workers.len();
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, snapshot);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let done = dashboard.panels[Panel::Done as usize].unwrap();
    assert!(!labels_under(&dashboard.tree, done).contains(&"該当なし".to_owned()));

    slash(&mut dashboard, &mut text);
    typed(&mut dashboard, &mut text, "\u{3000}");
    let count = format!("{running} / {running} 件");
    assert_eq!(count_label(&dashboard), Some(count), "the filter is on");
    let done = dashboard.panels[Panel::Done as usize].unwrap();
    let labels = labels_under(&dashboard.tree, done);
    assert!(labels.contains(&"該当なし".to_owned()), "{labels:?}");
    assert_eq!(panes_in(&dashboard, Panel::Running).len(), running);
}
