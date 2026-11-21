use crate::fleet::{Phase, Worker};
use noa_gpui_core::widgets::Label;

use super::*;

#[test]
fn a_long_lane_label_is_cut_inside_its_column() {
    let mut fleet = fleet::canned();
    fleet.workers[0].label = "noa-gpui ウィジェットによるUI拡張案".into();
    let mut dashboard = Dashboard::of_snapshot(Grouping::Flat, live(fleet, 0));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let tree = &dashboard.tree;
    let lanes = dashboard.timeline.as_ref().unwrap().lanes;
    let column = tree.children(tree.children(lanes)[0])[0];
    let label = tree.children(column)[0];
    let shown = tree.get::<Label>(label).unwrap();
    assert_ne!(shown.shown_text(), shown.text());
    assert!(shown.shown_text().ends_with('…'), "{}", shown.shown_text());
    let (column, label) = (rect(tree, column), rect(tree, label));
    assert!(
        label[0] >= column[0] - 0.5 && label[0] + label[2] <= column[0] + column[2] + 0.5,
        "label {label:?} is not inside its column {column:?}"
    );
}

/// Two lanes whose phases are given in seconds.
fn fleet_in_seconds(lanes: [&[(Phase, f64)]; 2]) -> Fleet {
    let canned = fleet::canned();
    let workers = lanes
        .iter()
        .zip(&canned.workers)
        .map(|(phases, worker)| Worker {
            phases: phases.iter().map(|(p, secs)| (*p, secs / 60.0)).collect(),
            ..worker.clone()
        })
        .collect();
    Fleet {
        workers,
        orchestrators: canned.orchestrators,
    }
}

#[test]
fn lanes_reach_now_whatever_the_span() {
    let cases: [[&[(Phase, f64)]; 2]; 2] = [
        [
            &[
                (Phase::Running, 20.0),
                (Phase::Permission, 10.0),
                (Phase::Running, 9.0),
            ],
            &[(Phase::Running, 3.0)],
        ],
        [
            &[(Phase::Running, 2.0), (Phase::Input, 1.5)],
            &[(Phase::Quiet, 0.5), (Phase::Ended, 1.0)],
        ],
    ];
    for lanes in cases {
        let fleet = fleet_in_seconds(lanes);
        let span: f64 = lanes[0].iter().map(|(_, secs)| secs).sum();
        let mut dashboard = Dashboard::of_snapshot(Grouping::Flat, live(fleet, 0));
        let mut text = TextStore::new(Default::default());
        dashboard.prepare(window(), &mut text);
        let tree = &dashboard.tree;
        let rows = tree.children(dashboard.timeline.as_ref().unwrap().lanes);
        assert_eq!(rows.len(), 2);
        for (row, phases) in rows.iter().zip(lanes) {
            let track = rect(tree, tree.children(*row)[1]);
            let segments: Vec<[f32; 4]> = tree
                .children(tree.children(*row)[1])
                .iter()
                .map(|s| rect(tree, *s))
                .collect();
            let last = segments.last().unwrap();
            assert!(
                (last[0] + last[2] - (track[0] + track[2])).abs() <= 0.5,
                "span {span} s: lane ends at {} of a track ending at {}",
                last[0] + last[2],
                track[0] + track[2]
            );
            let lane: f64 = phases.iter().map(|(_, secs)| secs).sum();
            let mut durations: Vec<f64> = phases.iter().map(|(_, secs)| *secs).collect();
            if lane < span {
                durations.insert(0, span - lane);
            }
            assert_eq!(segments.len(), durations.len());
            for (segment, secs) in segments.iter().zip(&durations) {
                let expected = track[2] * (*secs / span) as f32;
                assert!(
                    (segment[2] - expected).abs() <= 0.5,
                    "span {span} s: a {secs} s segment is {} px, expected {expected}",
                    segment[2]
                );
            }
        }
    }
}
