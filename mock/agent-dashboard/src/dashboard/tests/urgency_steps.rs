use super::*;
use crate::dashboard::style::RED;
use crate::dashboard::urgency;
use crate::fleet::Need;
use noa_gpui_scene::Color;

fn worker_with(need_of: u64, minutes: f64) -> Worker {
    let mut worker = fleet::canned().worker(need_of).unwrap().clone();
    worker.wait_minutes = minutes;
    worker
}

#[test]
fn steps_follow_the_thresholds() {
    // spec-review (16) waits on a question, e2e-smoke (19) on a failure,
    // fix-tests (14) on nothing yet: only the wait and the need matter.
    let steps = |need: Need, minutes: [f64; 4]| {
        let worker = worker_with(16, 0.0);
        minutes.map(|m| {
            urgency::step(
                &Worker {
                    wait_minutes: m,
                    ..worker.clone()
                },
                need,
            )
        })
    };
    for need in [Need::Approval, Need::Question, Need::Failure] {
        assert_eq!(
            steps(need, [4.9, 5.0, 14.9, 15.0]),
            [0, 1, 1, 2],
            "{need:?}"
        );
    }
    assert_eq!(steps(Need::Quiet, [9.9, 10.0, 29.9, 30.0]), [0, 1, 1, 2]);
    assert_eq!(steps(Need::Notice, [600.0; 4]), [0; 4]);
}

fn card_of(dashboard: &Dashboard, pane: u64) -> WidgetId {
    pulse_id(dashboard, |p| matches!(p, Pulse::QueueCard(x) if x == pane))
}

fn quad_of(dashboard: &Dashboard, pane: u64) -> (Color, Border) {
    let card = card_of(dashboard, pane);
    quad_at(dashboard, rect(&dashboard.tree, card)).expect("the card is drawn")
}

#[test]
fn a_card_looks_its_step() {
    let (dashboard, _) = laid_out(Grouping::Orchestrator);
    assert_eq!(
        quad_of(&dashboard, 16).1.width,
        2.0,
        "spec-review is step 1"
    );
    assert_eq!(quad_of(&dashboard, 14).1.width, 1.0, "fix-tests is step 0");

    let mut fleet = fleet::canned();
    fleet
        .workers
        .iter_mut()
        .find(|w| w.pane == 19)
        .unwrap()
        .wait_minutes = 20.0;
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet, 1));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let (fill, border) = quad_of(&dashboard, 19);
    assert_eq!(border.width, 3.0);
    assert_eq!(fill, RED.with_alpha(0.16));
}

#[test]
fn a_step_change_needs_no_rebuild() {
    let fleet = fleet::canned();
    let mut dashboard = Dashboard::of_snapshot(Grouping::Orchestrator, live(fleet.clone(), 1));
    let mut text = TextStore::new(Default::default());
    dashboard.prepare(window(), &mut text);
    let card = card_of(&dashboard, 15);
    assert_eq!(quad_of(&dashboard, 15).1.width, 1.0);

    let mut later = fleet;
    later
        .workers
        .iter_mut()
        .find(|w| w.pane == 15)
        .unwrap()
        .wait_minutes = 6.0;
    dashboard.apply(live(later, 2), &mut text);
    dashboard.prepare(window(), &mut text);

    assert_eq!(card_of(&dashboard, 15), card, "the card was not rebuilt");
    assert_eq!(quad_of(&dashboard, 15).1.width, 2.0);
}
