use super::*;

fn graph_labels(dashboard: &Dashboard) -> Vec<String> {
    let graph = dashboard.panels[Panel::Graph as usize].unwrap();
    labels_under(&dashboard.tree, graph)
}

#[test]
fn the_orchestrator_is_the_graph_root_not_its_own_child() {
    let fleet = crate::live::tests::reported_fleet();
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let labels = graph_labels(&dashboard);
    let at = |name: &str| labels.iter().position(|l| l == name).unwrap();
    let named = labels
        .iter()
        .filter(|l| *l == REPORTED_ORCHESTRATOR)
        .count();
    assert_eq!(named, 1, "{labels:?}");
    // The root carries its own pane's state: running.
    assert_eq!(labels[at(REPORTED_ORCHESTRATOR) - 1], "●", "{labels:?}");
    assert!(at("demo-sleep") > at(REPORTED_ORCHESTRATOR), "{labels:?}");
    assert!(at("demo-agent") > at(REPORTED_ORCHESTRATOR), "{labels:?}");
    assert_eq!(labels[at("単独") - 1], "◆", "{labels:?}");
    assert!(at("Claude Code") > at("単独"), "{labels:?}");
    assert_eq!(labels.iter().filter(|l| *l == "◆").count(), 1);
}

#[test]
fn the_graph_keeps_roots_with_one_name_apart() {
    let fleet = crate::live::tests::same_named_roots_fleet();
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let tree = &dashboard.tree;
    let graph = dashboard.panels[Panel::Graph as usize].unwrap();
    let roots = tree.children(graph)[1];
    let rows: Vec<Vec<String>> = tree
        .children(roots)
        .iter()
        .map(|row| labels_under(tree, *row))
        .collect();
    assert_eq!(
        rows,
        [
            ["●", "Claude Code", "●", "w-a"],
            ["●", "Claude Code", "●", "w-b"],
            ["◆", "zsh", "●", "w-c"],
            ["◆", "zsh", "●", "w-d"],
        ]
    );
}
