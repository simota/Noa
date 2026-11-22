use crate::dashboard::analytics::compute_metrics;
use crate::dashboard::gantt::{TimeScale, compute_concurrency};
use crate::dashboard::mindmap::MapAxis;

use super::*;

#[test]
fn t_key_cycles_through_all_four_tabs() {
    let (mut dashboard, mut text) = laid_out(Grouping::Orchestrator);
    assert_eq!(dashboard.view.tab, Tab::Operations);
    for expected in [Tab::Analytics, Tab::MindMap, Tab::Timeline, Tab::Operations] {
        key(&mut dashboard, &mut text, Key::Char('t'));
        assert_eq!(dashboard.view.tab, expected);
        assert_eq!(notice_of(&dashboard), format!("表示: {}", expected.title()));
    }
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
