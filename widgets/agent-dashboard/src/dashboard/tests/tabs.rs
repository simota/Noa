use crate::dashboard::analytics::compute_metrics;
use crate::dashboard::gantt::{TimeScale, compute_concurrency};
use crate::dashboard::mindmap::MapAxis;

use noa_gpui_core::accesskit::Action as Access;
use noa_gpui_core::widgets::{Button, SegmentedControl};
use noa_gpui_core::{access_id, access_item_id};

use super::hardening::{assistive, pick_segment};
use super::*;

/// Presses `button` as assistive technology does (no layout needed).
fn press_button(dashboard: &mut Dashboard, text: &mut TextStore, button: WidgetId) {
    assistive(dashboard, text, access_id(button).0, Access::Click);
}

/// Shows `tab` as a person reaches it: 分析 from the list, then the
/// analysis tabs' switch; 一覧へ戻る back to the list.
pub(super) fn show_tab(dashboard: &mut Dashboard, text: &mut TextStore, tab: Tab) {
    if tab == Tab::Operations {
        if dashboard.view.tab != Tab::Operations {
            let back = dashboard.registry.back_button.expect("一覧へ戻る");
            press_button(dashboard, text, back);
        }
    } else {
        if dashboard.view.tab == Tab::Operations {
            let analysis = dashboard.registry.analysis_button.expect("分析");
            press_button(dashboard, text, analysis);
        }
        if dashboard.view.tab != tab {
            let control = dashboard.registry.tab_control.expect("the analysis switch");
            let at = Tab::ANALYSIS.iter().position(|t| *t == tab).unwrap();
            pick_segment(dashboard, text, control, at);
        }
    }
    assert_eq!(dashboard.view.tab, tab);
}

fn button_text(dashboard: &Dashboard, id: WidgetId) -> String {
    dashboard.tree.get::<Button>(id).unwrap().label().to_owned()
}

/// The segment titles of `control`, as assistive technology reads them.
fn segments(dashboard: &Dashboard, control: WidgetId) -> Vec<String> {
    let count = dashboard
        .tree
        .get::<SegmentedControl>(control)
        .unwrap()
        .selected()
        .len();
    let update = dashboard.tree.accessibility_update(1.0);
    (0..count)
        .map(|item| {
            let id = access_item_id(control, item);
            let (_, node) = update.nodes.iter().find(|(node, _)| *node == id).unwrap();
            node.label().unwrap_or_default().to_owned()
        })
        .collect()
}

/// Every segmented control in the tree, overlays aside.
fn segmented_controls(dashboard: &Dashboard) -> Vec<WidgetId> {
    fn walk(tree: &UiTree, id: WidgetId, out: &mut Vec<WidgetId>) {
        if tree.get::<SegmentedControl>(id).is_some() {
            out.push(id);
        }
        for child in tree.children(id) {
            walk(tree, *child, out);
        }
    }
    let mut out = Vec::new();
    walk(&dashboard.tree, dashboard.tree.root().unwrap(), &mut out);
    out
}

#[test]
fn t_key_cycles_through_all_four_tabs() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    assert_eq!(dashboard.view.tab, Tab::Operations);
    assert_eq!(Tab::Operations.title(), "一覧");
    for expected in [Tab::Analytics, Tab::MindMap, Tab::Timeline, Tab::Operations] {
        key(&mut dashboard, &mut text, Key::Char('t'));
        assert_eq!(dashboard.view.tab, expected);
        assert_eq!(notice_of(&dashboard), format!("表示: {}", expected.title()));
    }
    assert_eq!(notice_of(&dashboard), "表示: 一覧");
}

/// dashboard-worker-list AC10: the list's toolbar ends with 分析, which
/// opens 統計; the analysis tabs' first row has 一覧へ戻る, which returns
/// to the list as it was.
#[test]
fn analysis_opens_the_statistics_and_back_returns_to_the_list() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    in_scope(&mut dashboard, &mut text, Scope::Attention);
    let analysis = dashboard.registry.analysis_button.expect("分析");
    assert_eq!(button_text(&dashboard, analysis), "分析");
    let list = dashboard.registry.list.as_ref().unwrap().list;
    let toolbar = dashboard
        .tree
        .children(dashboard.tree.parent(list).unwrap())[0];
    assert_eq!(
        dashboard.tree.parent(analysis),
        Some(toolbar),
        "分析 is in the list's toolbar"
    );
    let last_shown = dashboard
        .tree
        .children(toolbar)
        .iter()
        .rev()
        .find(|id| !dashboard.tree.is_hidden(**id))
        .copied();
    assert_eq!(last_shown, Some(analysis), "the toolbar ends with 分析");
    assert!(dashboard.registry.back_button.is_none());

    // A pointer click, as a person makes it.
    let pos = center(&dashboard.tree, analysis);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.tab, Tab::Analytics);
    let labels = labels_under(&dashboard.tree, dashboard.body);
    assert!(labels.iter().any(|l| l == "フリート稼働状況"), "{labels:?}");
    assert!(dashboard.registry.list.is_none(), "the list is not built");
    assert!(dashboard.registry.analysis_button.is_none());
    let back = dashboard.registry.back_button.expect("一覧へ戻る");
    assert_eq!(button_text(&dashboard, back), "一覧へ戻る");
    // The row is the body's first.
    let first = dashboard.tree.children(dashboard.body)[0];
    assert_eq!(dashboard.tree.parent(back), Some(first));

    let pos = center(&dashboard.tree, back);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.tab, Tab::Operations);
    assert_eq!(dashboard.view.scope, Scope::Attention, "the scope stays");
    assert!(dashboard.registry.list.is_some());
    assert!(dashboard.registry.back_button.is_none());
}

/// The analysis row's switch reaches 統計, マップ and タイムライン; the
/// footer holds no tab switch any more.
#[test]
fn the_analysis_switch_reaches_every_analysis_tab() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let notice_bar = dashboard
        .tree
        .parent(dashboard.tree.parent(dashboard.notice).unwrap());
    let in_footer = |dashboard: &Dashboard| {
        segmented_controls(dashboard)
            .into_iter()
            .filter(|id| dashboard.tree.parent(*id) == notice_bar)
            .count()
    };
    assert_eq!(in_footer(&dashboard), 0, "no tab switch in the footer");
    for tab in [Tab::Timeline, Tab::MindMap, Tab::Analytics] {
        show_tab(&mut dashboard, &mut text, tab);
        let control = dashboard.registry.tab_control.unwrap();
        assert_eq!(
            segments(&dashboard, control),
            ["統計", "マップ", "タイムライン"]
        );
        assert_eq!(in_footer(&dashboard), 0, "{tab:?}");
    }
    show_tab(&mut dashboard, &mut text, Tab::Operations);
}

/// The grouping control orders the analysis views only: it is in their
/// row, never in the header or the list.
#[test]
fn the_grouping_control_exists_only_in_analysis_tabs() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    let grouping_titles: Vec<String> = Grouping::ALL.iter().map(|g| g.title().into()).collect();
    let groupings = |dashboard: &Dashboard| -> Vec<WidgetId> {
        segmented_controls(dashboard)
            .into_iter()
            .filter(|id| segments(dashboard, *id) == grouping_titles)
            .collect()
    };
    let caption = |dashboard: &Dashboard| {
        labels_under(&dashboard.tree, dashboard.tree.root().unwrap())
            .iter()
            .any(|l| l == "グループ")
    };
    assert!(dashboard.registry.grouping_control.is_none());
    assert!(
        groupings(&dashboard).is_empty(),
        "a grouping control on 一覧"
    );
    assert!(!caption(&dashboard), "グループ on 一覧");
    for tab in Tab::ANALYSIS {
        show_tab(&mut dashboard, &mut text, tab);
        let control = dashboard.registry.grouping_control.expect("グループ");
        assert_eq!(groupings(&dashboard), [control], "{tab:?}");
        assert!(caption(&dashboard), "{tab:?}");
        let row = dashboard.tree.children(dashboard.body)[0];
        assert_eq!(dashboard.tree.parent(control), Some(row), "{tab:?}");
    }
    // It still groups: the last segment.
    let control = dashboard.registry.grouping_control.unwrap();
    pick_segment(&mut dashboard, &mut text, control, Grouping::ALL.len() - 1);
    assert_eq!(dashboard.view.grouping, Grouping::Flat);
    show_tab(&mut dashboard, &mut text, Tab::Operations);
    assert!(groupings(&dashboard).is_empty());
}

#[test]
fn analytics_computes_wait_ratio_and_throughput_accurately() {
    let fleet = fleet::canned();
    let refs: Vec<&Worker> = fleet.workers.iter().collect();
    let metrics = compute_metrics(&refs);
    assert_eq!(metrics.total, 9);
    assert_eq!(metrics.running, 3);
    assert_eq!(metrics.finished, 2);
    assert_eq!(metrics.error, 1);
    assert_eq!(metrics.results_count, 2);
    assert!(metrics.total_minutes > 0.0);
    assert!(metrics.waited_minutes > 0.0);
    assert!(metrics.wait_ratio_pct > 0.0 && metrics.wait_ratio_pct < 100.0);
    assert!(!metrics.throughput.is_empty());

    // Analytics tab builds and lays out cleanly across window sizes.
    for size in [Size::new(1100.0, 700.0), Size::new(1440.0, 900.0)] {
        let mut dashboard = Dashboard::new(Grouping::Orchestrator).with_tab(Tab::Analytics);
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(size, &mut text);
        let labels = labels_under(&dashboard.tree, dashboard.body);
        assert!(labels.iter().any(|l| l == "フリート稼働状況"));
        assert!(
            labels
                .iter()
                .any(|l| l == "フェーズ別 時間内訳（フリート合算）")
        );
        assert!(labels.iter().any(|l| l == "エージェント・モデル別 内訳"));
    }
}

#[test]
fn mindmap_switches_axes_and_detects_branch_conflict_and_opens_drawer_on_click() {
    let mut fleet = fleet::canned();
    // Put two active workers on the same branch to trigger conflict warning.
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 12)
        .unwrap()
        .branch = "feat/api-v2".into();
    let mut dashboard =
        Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0)).with_tab(Tab::MindMap);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    assert_eq!(dashboard.view.map_axis, MapAxis::Orchestrator);

    // Click a node in MindMap tab: selects it and opens its details drawer.
    let node_11 = dashboard
        .registry
        .marks
        .iter()
        .find(|(_, pane, mark)| *pane == 11 && matches!(mark, Mark::Node { .. }))
        .unwrap()
        .0;
    let pos = center(&dashboard.tree, node_11);
    click(&mut dashboard, &mut text, pos);
    assert_eq!(dashboard.view.selected, Some(11));
    assert_eq!(dashboard.view.detail, Some(11));

    // Switch to Branch axis and confirm concurrency conflict tag appears.
    let view = View {
        map_axis: MapAxis::Branch,
        detail: None,
        ..dashboard.view
    };
    dashboard.change_view(view, &mut text);
    dashboard.prepare(window(), &mut text);
    let tags = tags_under(&dashboard.tree, dashboard.body);
    assert!(
        tags.iter().any(|t| t.contains("競合注意")),
        "expected conflict tag in {tags:?}"
    );

    // Switch to State axis.
    let view = View {
        map_axis: MapAxis::State,
        ..dashboard.view
    };
    dashboard.change_view(view, &mut text);
    dashboard.prepare(window(), &mut text);
    let labels = labels_under(&dashboard.tree, dashboard.body);
    assert!(labels.iter().any(|l| l == "判断待ち"));
    assert!(labels.iter().any(|l| l == "作業中"));
}

#[test]
fn gantt_computes_concurrency_and_switches_time_scales() {
    let fleet = fleet::canned();
    let refs: Vec<&Worker> = fleet.workers.iter().collect();
    let conc = compute_concurrency(&refs, 41.0);
    assert_eq!(conc.running.len(), 24);
    assert!(conc.peak_running >= 1);
    assert!(conc.peak_waiting >= 1);

    let mut dashboard = Dashboard::new(Grouping::Orchestrator).with_tab(Tab::Timeline);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    for scale in TimeScale::ALL {
        let view = View {
            time_scale: scale,
            ..dashboard.view
        };
        dashboard.change_view(view, &mut text);
        dashboard.prepare(window(), &mut text);
        let labels = labels_under(&dashboard.tree, dashboard.body);
        assert!(labels.iter().any(|l| l == "タイムライン・並列度"));
        assert!(labels.iter().any(|l| l == "同時稼働数（作業中）"));
    }
}

#[test]
fn tab_parse_accepts_cli_names() {
    assert_eq!(Tab::parse("ops"), Some(Tab::Operations));
    assert_eq!(Tab::parse("analytics"), Some(Tab::Analytics));
    assert_eq!(Tab::parse("map"), Some(Tab::MindMap));
    assert_eq!(Tab::parse("timeline"), Some(Tab::Timeline));
    assert_eq!(Tab::parse("unknown"), None);
}

/// The canned fleet with 600 more minutes waited on approval by worker 14:
/// the same structure, so a live dashboard takes it in place.
fn later_fleet() -> Fleet {
    let mut fleet = fleet::canned();
    let worker = fleet.workers.iter_mut().find(|w| w.pane == 14).unwrap();
    worker.phases.push((fleet::Phase::Permission, 600.0));
    worker.wait_minutes += 600.0;
    fleet
}

fn live_on(tab: Tab, size: Size) -> (Dashboard, TextStore, std::sync::mpsc::Sender<Snapshot>) {
    let (channel, snapshots, _notices, _commands) = Channel::detached();
    let mut dashboard = Dashboard::live(Grouping::Orchestrator, channel).with_tab(tab);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(size, &mut text);
    snapshots.send(live(fleet::canned(), 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(size, &mut text);
    (dashboard, text, snapshots)
}

fn scroll_views(tree: &UiTree, id: WidgetId, out: &mut Vec<WidgetId>) {
    if tree.get::<noa_gpui_core::widgets::ScrollView>(id).is_some() {
        out.push(id);
    }
    for child in tree.children(id) {
        scroll_views(tree, *child, out);
    }
}

/// Statistics shows what time changed (the waited total) once a live
/// snapshot of the same structure comes, and keeps its scroll position.
#[test]
fn analytics_follows_a_live_snapshot_of_the_same_structure() {
    let size = Size::new(1100.0, 600.0);
    let (mut dashboard, mut text, snapshots) = live_on(Tab::Analytics, size);
    let mut views = Vec::new();
    scroll_views(&dashboard.tree, dashboard.body, &mut views);
    let scroll = views[0];
    dashboard
        .tree
        .update::<noa_gpui_core::widgets::ScrollView, _>(scroll, |s, cx| s.set_offset(cx, 80.0));
    dashboard.prepare(size, &mut text);
    let offset = |d: &Dashboard| {
        let mut views = Vec::new();
        scroll_views(&d.tree, d.body, &mut views);
        d.tree
            .get::<noa_gpui_core::widgets::ScrollView>(views[0])
            .unwrap()
            .offset()
    };
    let before = offset(&dashboard);
    assert!(
        before > 0.0,
        "the statistics overflow the window and scrolled"
    );

    let later = later_fleet();
    assert!(fleet::canned().same_structure(&later));
    let refs: Vec<&Worker> = later.workers.iter().collect();
    let metrics = compute_metrics(&refs);
    let expected = format!(
        "人待ち合計 {} / 延べ {}",
        fleet::minutes_text(metrics.waited_minutes),
        fleet::minutes_text(metrics.total_minutes)
    );
    snapshots.send(live(later, 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(size, &mut text);
    let labels = labels_under(&dashboard.tree, dashboard.body);
    assert!(labels.contains(&expected), "{expected:?} in {labels:?}");
    assert_eq!(offset(&dashboard), before, "scroll position kept");
}

/// The timeline tab's lanes show the new total once a live snapshot of the
/// same structure comes.
#[test]
fn the_timeline_tab_follows_a_live_snapshot_of_the_same_structure() {
    let (mut dashboard, mut text, snapshots) = live_on(Tab::Timeline, window());
    let later = later_fleet();
    let expected = fleet::minutes_text(later.worker(14).unwrap().total_minutes());
    snapshots.send(live(later, 0)).unwrap();
    dashboard.before_frame(&mut text);
    dashboard.prepare(window(), &mut text);
    let labels = labels_under(&dashboard.tree, dashboard.body);
    assert!(labels.contains(&expected), "{expected:?} in {labels:?}");
}

/// The map's tree, filtered, leaves out the children that neither match
/// nor lead to a match, and an orchestrator's own pane is clickable only
/// when it matches.
#[test]
fn the_map_prunes_unmatched_descendants_and_unmatched_orchestrator_panes() {
    // 11 (running) matches state:running; its children 14 and 17 do not.
    let mut dashboard = Dashboard::new(Grouping::Orchestrator)
        .with_tab(Tab::MindMap)
        .with_filter("state:running".into());
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let labels = labels_under(&dashboard.tree, dashboard.body);
    for gone in ["fix-tests", "lint-pass"] {
        assert!(!labels.iter().any(|l| l == gone), "{gone} in {labels:?}");
    }
    assert!(labels.iter().any(|l| l == "api-refactor"));

    // release-0.9.7 as the open pane 16 (input), its workers under it.
    let mut fleet = fleet::canned();
    let release = fleet
        .orchestrators
        .iter_mut()
        .find(|o| o.name == "release-0.9.7")
        .unwrap();
    release.pane = Some(16);
    for worker in &mut fleet.workers {
        if worker.orchestrator == "release-0.9.7" && worker.pane != 16 {
            worker.orchestrator_pane = Some(16);
        }
    }
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0))
        .with_tab(Tab::MindMap)
        .with_filter("state:running".into());
    dashboard.prepare(window(), &mut text);
    assert!(
        !dashboard
            .registry
            .marks
            .iter()
            .any(|(_, pane, _)| *pane == 16),
        "16 does not match state:running"
    );
}
