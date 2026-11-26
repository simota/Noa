//! One definition of 要対応 (`Worker::needs_attention`) behind the search's
//! `state:waiting`, the statistics' count and the map's hub counts.

use std::collections::{BTreeMap, BTreeSet};

use noa_gpui_core::widgets::Badge;

use crate::dashboard::analytics::compute_metrics;
use crate::dashboard::filter::matches;
use crate::dashboard::mindmap::MapAxis;

use super::*;

/// One worker of each kind: approvals (14, 15), an input (16), a failure
/// (19), running (11), quiet (13), a notice on a running worker (12) and
/// on a finished one (17), finished (18), and closed finished and failed.
fn every_kind() -> Fleet {
    let mut fleet = fleet::canned_with_closed();
    for worker in &mut fleet.workers {
        if matches!(worker.pane, 12 | 17) {
            worker.attention = true;
        }
    }
    fleet.workers.last_mut().unwrap().state = State::Error;
    fleet
}

fn attention_set(fleet: &Fleet) -> BTreeSet<u64> {
    fleet
        .workers
        .iter()
        .filter(|w| w.needs_attention())
        .map(|w| w.pane)
        .collect()
}

#[test]
fn every_kind_has_what_the_tests_need() {
    let fleet = every_kind();
    let needs: BTreeSet<_> = fleet
        .workers
        .iter()
        .filter_map(|w| w.need())
        .map(|n| format!("{n:?}"))
        .collect();
    for title in ["Approval", "Question", "Failure", "Notice", "Quiet"] {
        assert!(needs.contains(title), "{title} in {needs:?}");
    }
    assert!(
        fleet
            .workers
            .iter()
            .any(|w| w.is_closed() && w.state == State::Error)
    );
    assert_eq!(attention_set(&fleet), BTreeSet::from([14, 15, 16, 19]));
}

#[test]
fn state_waiting_is_the_attention_set() {
    let fleet = every_kind();
    for query in ["state:waiting", "state:要対応", "s:waiting"] {
        let found: BTreeSet<u64> = fleet
            .workers
            .iter()
            .filter(|w| matches(w, query))
            .map(|w| w.pane)
            .collect();
        assert_eq!(found, attention_set(&fleet), "{query}");
    }
    // `need:any` keeps its meaning: any need, notices and quiet included.
    let any: BTreeSet<u64> = fleet
        .workers
        .iter()
        .filter(|w| matches(w, "need:any"))
        .map(|w| w.pane)
        .collect();
    assert_eq!(any, BTreeSet::from([12, 13, 14, 15, 16, 17, 19]));
}

#[test]
fn analytics_waiting_counts_the_attention_set() {
    let fleet = every_kind();
    let refs: Vec<&Worker> = fleet.workers.iter().collect();
    assert_eq!(compute_metrics(&refs).waiting, attention_set(&fleet).len());
}

#[test]
fn branch_and_cwd_qualifiers_match() {
    let fleet = every_kind();
    let found = |query: &str| -> BTreeSet<u64> {
        fleet
            .workers
            .iter()
            .filter(|w| matches(w, query))
            .map(|w| w.pane)
            .collect()
    };
    assert_eq!(found("branch:perf/reflow"), BTreeSet::from([13]));
    assert_eq!(found("b:FIX/flaky"), BTreeSet::from([14]));
    assert_eq!(found("cwd:noa-wt/perf-probe"), BTreeSet::from([13]));
    assert_eq!(
        found("c:/work/noa-wt/BENCH-ascii"),
        BTreeSet::from([(1 << 63) | 1])
    );
    assert_eq!(found("-cwd:/work/noa-wt").len(), 1, "docs-typo has no cwd");
    // A plain term never reads the cwd: every canned cwd holds "/work".
    assert!(found("/work").is_empty());
}

/// The 要対応 count a map hub titled `title` shows (0 without the tag), or
/// `None` when no hub has that title. A hub is a container whose first
/// child (its head) holds the title and the member count's badge.
fn hub_waiting(tree: &UiTree, id: WidgetId, title: &str) -> Option<usize> {
    let children = tree.children(id);
    if let Some(head) = children.first() {
        let is_hub = tree
            .children(*head)
            .iter()
            .any(|c| tree.get::<Badge>(*c).is_some())
            && labels_under(tree, *head).iter().any(|l| l == title);
        if is_hub {
            let waiting = children
                .iter()
                .filter_map(|c| tree.get::<Tag>(*c))
                .find_map(|t| t.text().strip_prefix("要対応 ")?.parse().ok());
            return Some(waiting.unwrap_or(0));
        }
    }
    children
        .iter()
        .find_map(|child| hub_waiting(tree, *child, title))
}

fn map_of(fleet: &Fleet, axis: MapAxis) -> (Dashboard, TextStore) {
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet.clone(), 0))
        .with_tab(Tab::MindMap);
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let view = View {
        map_axis: axis,
        ..dashboard.view
    };
    dashboard.change_view(view, &mut text);
    dashboard.prepare(window(), &mut text);
    (dashboard, text)
}

#[test]
fn the_orchestrator_hubs_count_the_attention_set() {
    let fleet = every_kind();
    let (dashboard, _text) = map_of(&fleet, MapAxis::Orchestrator);
    let mut seen = 0;
    for orchestrator in &fleet.orchestrators {
        // A hub counts the workers hung on it directly.
        let expected = fleet
            .workers
            .iter()
            .filter(|w| w.orchestrator == orchestrator.name && w.parent.is_none())
            .filter(|w| w.orchestrator_pane == orchestrator.pane)
            .filter(|w| w.needs_attention())
            .count();
        let shown = hub_waiting(&dashboard.tree, dashboard.body, &orchestrator.name);
        if let Some(shown) = shown {
            seen += 1;
            assert_eq!(shown, expected, "{}", orchestrator.name);
        }
    }
    assert!(seen >= 2, "the map draws the orchestrators' hubs");
}

#[test]
fn the_branch_hubs_count_the_attention_set() {
    let fleet = every_kind();
    let (dashboard, _text) = map_of(&fleet, MapAxis::Branch);
    let mut groups: BTreeMap<String, usize> = BTreeMap::new();
    for worker in &fleet.workers {
        let branch = if worker.branch.is_empty() {
            "（ブランチ不明）".to_owned()
        } else {
            worker.branch.clone()
        };
        *groups.entry(branch).or_default() += usize::from(worker.needs_attention());
    }
    for (branch, expected) in &groups {
        assert_eq!(
            hub_waiting(&dashboard.tree, dashboard.body, branch),
            Some(*expected),
            "{branch}"
        );
    }
    // Quiet (13) and a notice (12) used to count here.
    assert_eq!(
        hub_waiting(&dashboard.tree, dashboard.body, "perf/reflow"),
        Some(0)
    );
    assert_eq!(
        hub_waiting(&dashboard.tree, dashboard.body, "docs/sync"),
        Some(0)
    );
}
