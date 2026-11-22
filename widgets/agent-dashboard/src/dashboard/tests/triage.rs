use std::path::PathBuf;

use noa_gpui_core::widgets::Badge;

use crate::fleet::Tier;

use super::*;

fn queue_labels(dashboard: &Dashboard) -> Vec<String> {
    let queue = dashboard.panels[Panel::Queue as usize].unwrap();
    labels_under(&dashboard.tree, queue)
}

fn queue_panes(dashboard: &Dashboard) -> Vec<u64> {
    let list = dashboard.registry.lists[Panel::Queue as usize].as_ref();
    list.unwrap().items.iter().filter_map(|(_, p)| *p).collect()
}

fn badge_of(dashboard: &Dashboard) -> Option<String> {
    let queue = dashboard.panels[Panel::Queue as usize].unwrap();
    let title = dashboard.tree.children(queue)[0];
    let tree = &dashboard.tree;
    let badge = tree
        .children(title)
        .iter()
        .find_map(|id| tree.get::<Badge>(*id));
    badge.and_then(Badge::label)
}

fn click_info(dashboard: &mut Dashboard, text: &mut TextStore) {
    let toggle = dashboard.queue_buttons.info_toggle.unwrap();
    let pos = center(&dashboard.tree, toggle);
    click(dashboard, text, pos);
}

#[test]
fn the_queue_puts_decisions_first_and_folds_what_only_informs() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    assert_eq!(queue_panes(&dashboard), [14, 15, 16, 19]);
    let labels = queue_labels(&dashboard);
    assert!(labels.contains(&"判断待ち · 3".to_owned()), "{labels:?}");
    assert!(labels.contains(&"要確認 · 1".to_owned()), "{labels:?}");
    assert!(!labels.iter().any(|l| l.starts_with("参考情報 ·")));
    assert_eq!(
        badge_of(&dashboard).as_deref(),
        Some("4"),
        "13 is not counted"
    );
    assert_eq!(dashboard.queue_buttons.expanded, Some(14));

    click_info(&mut dashboard, &mut text);
    assert!(dashboard.view.info_open);
    assert_eq!(queue_panes(&dashboard), [14, 15, 16, 19, 13]);
    assert!(queue_labels(&dashboard).contains(&"参考情報 · 1".to_owned()));
    assert_eq!(badge_of(&dashboard).as_deref(), Some("4"));
    assert_eq!(dashboard.queue_buttons.expanded, Some(14), "not 13");

    click_info(&mut dashboard, &mut text);
    assert!(!dashboard.view.info_open);
    assert_eq!(queue_panes(&dashboard), [14, 15, 16, 19]);
}

#[test]
fn what_only_informs_is_never_expanded_on_its_own() {
    let mut fleet = fleet::canned();
    fleet
        .workers
        .retain(|w| w.need().is_none_or(|n| n.tier() == Tier::Info));
    let mut dashboard = Dashboard::showing(View {
        info_open: true,
        ..View::of(Grouping::Orchestrator)
    });
    let mut text = TextStore::new(Default::default());
    dashboard.apply(live(fleet, 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(queue_panes(&dashboard), [13]);
    assert_eq!(dashboard.queue_buttons.expanded, None);
    assert_eq!(badge_of(&dashboard), Badge::count(0).label());
}

#[test]
fn an_open_info_section_reorders_in_place_as_a_build_shows_it() {
    let view = View {
        info_open: true,
        ..View::of(Grouping::Orchestrator)
    };
    let mut dashboard = Dashboard::showing(view);
    let mut text = TextStore::new(Default::default());
    dashboard.apply(live(fleet::canned(), 0), &mut text);
    dashboard.prepare(window(), &mut text);
    let mut fleet = fleet::canned();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 15)
        .unwrap()
        .wait_minutes = 3.0;
    let structure = dashboard.queue_buttons.details.clone();
    dashboard.apply(live(fleet.clone(), 0), &mut text);
    dashboard.prepare(window(), &mut text);
    assert_eq!(queue_panes(&dashboard), [15, 14, 16, 19, 13]);
    assert_eq!(dashboard.queue_buttons.expanded, Some(15));
    assert_ne!(
        dashboard.queue_buttons.details, structure,
        "expanded in place"
    );

    let mut fresh = Dashboard::showing(view);
    let mut fresh_text = TextStore::new(Default::default());
    fresh.apply(live(fleet, 0), &mut fresh_text);
    fresh.prepare(window(), &mut fresh_text);
    assert_eq!(
        frame_of(dashboard.display_list(), &text),
        frame_of(fresh.display_list(), &fresh_text)
    );
}

/// A file under the temporary directory, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, contents: &[u8]) -> Self {
        let path =
            std::env::temp_dir().join(format!("agent-dashboard-{}-{name}", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        TempFile(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The canned fleet on a live link, `docs-sync` (17, done) leaving `result`;
/// its drawer open.
fn drawer_on_result(result: PathBuf, can_focus: bool) -> (Dashboard, TextStore) {
    let mut fleet = fleet::canned();
    let done = fleet.workers.iter_mut().find(|w| w.pane == 17).unwrap();
    done.result = Some(result);
    let mut dashboard = Dashboard::of_snapshot(
        Grouping::Orchestrator,
        Snapshot {
            can_focus,
            ..live(fleet, 0)
        },
    );
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    dashboard.set_detail(Some(17), &mut text);
    dashboard.prepare(window(), &mut text);
    (dashboard, text)
}

fn drawer_texts(dashboard: &Dashboard) -> Vec<String> {
    let (root, _) = dashboard.registry.drawer.unwrap();
    labels_under(&dashboard.tree, root)
}

fn toggle_result(dashboard: &mut Dashboard, text: &mut TextStore) {
    let toggle = dashboard.registry.result_toggle.expect("a result button");
    let pos = center(&dashboard.tree, toggle);
    click(dashboard, text, pos);
}

#[test]
fn the_drawer_reads_the_result_only_when_asked() {
    let file = TempFile::new("result.md", b"# Review\nall green\n");
    let (mut dashboard, mut text) = drawer_on_result(file.0.clone(), true);
    assert!(!drawer_texts(&dashboard).contains(&"all green".to_owned()));

    toggle_result(&mut dashboard, &mut text);
    assert_eq!(dashboard.view.result, Some(17));
    let texts = drawer_texts(&dashboard);
    assert!(texts.contains(&"# Review".to_owned()), "{texts:?}");
    assert!(texts.contains(&"all green".to_owned()), "{texts:?}");

    toggle_result(&mut dashboard, &mut text);
    assert_eq!(dashboard.view.result, None);
    assert!(!drawer_texts(&dashboard).contains(&"all green".to_owned()));

    // Another worker's drawer shows no result read for 17.
    toggle_result(&mut dashboard, &mut text);
    dashboard.set_detail(Some(18), &mut text);
    assert!(!drawer_texts(&dashboard).contains(&"all green".to_owned()));
}

#[test]
fn a_result_that_is_not_a_regular_file_is_not_read() {
    let dir = std::env::temp_dir();
    let (mut dashboard, mut text) = drawer_on_result(dir, true);
    toggle_result(&mut dashboard, &mut text);
    let texts = drawer_texts(&dashboard);
    let reason = "読めません: 通常のファイルではありません".to_owned();
    assert!(texts.contains(&reason), "{texts:?}");
}

#[test]
fn a_long_result_is_cut() {
    let line = "x".repeat(99);
    let long = format!("{line}\n").repeat(400);
    let file = TempFile::new("long.md", long.as_bytes());
    let (text, cut) = drawer::read_result(&file.0).unwrap();
    assert!(cut);
    assert!(text.len() as u64 <= drawer::RESULT_MAX_BYTES);
    assert!(text.lines().count() <= drawer::RESULT_MAX_LINES);
    let short = TempFile::new("short.md", b"done\n");
    assert_eq!(
        drawer::read_result(&short.0),
        Ok(("done".to_owned(), false))
    );
}

#[test]
fn the_drawer_names_the_pane_and_what_opening_it_needs() {
    let file = TempFile::new("scopes.md", b"ok\n");
    let (dashboard, _text) = drawer_on_result(file.0.clone(), false);
    let texts = drawer_texts(&dashboard);
    assert!(
        texts.iter().any(|t| t.starts_with("ID 17 · 親 ")),
        "{texts:?}"
    );
    assert!(
        texts.contains(&drawer::NEEDS_CONTROL.to_owned()),
        "{texts:?}"
    );

    let (dashboard, _text) = drawer_on_result(file.0.clone(), true);
    assert!(!drawer_texts(&dashboard).contains(&drawer::NEEDS_CONTROL.to_owned()));
}

#[test]
fn the_drawer_shows_more_of_the_screen_than_a_card() {
    let mut fleet = fleet::canned();
    let tail: Vec<String> = (0..20).map(|n| format!("line {n}")).collect();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 14)
        .unwrap()
        .screen_tail = tail;
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let card = queue_labels(&dashboard);
    assert!(card.contains(&"line 19".to_owned()));
    assert!(card.contains(&"line 14".to_owned()));
    assert!(!card.contains(&"line 13".to_owned()), "a card shows six");

    dashboard.set_detail(Some(14), &mut text);
    let texts = drawer_texts(&dashboard);
    assert!(texts.contains(&"line 0".to_owned()), "{texts:?}");
    assert!(texts.contains(&"line 19".to_owned()));
}
