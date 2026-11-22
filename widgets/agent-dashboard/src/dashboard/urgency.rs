//! Urgency steps of the queue cards (AC7): the longer a worker has waited on
//! a person, the heavier its card is drawn.
use noa_gpui_core::Role;
use noa_gpui_core::theme::Paint;
use noa_gpui_scene::Color;

use crate::fleet::{Need, Worker};

use super::style::need_color;

/// Alpha of the fill a step 2 card gets.
const FILL_ALPHA: f32 = 0.16;

/// How far along its wait a card is: 0 (as built), 1 or 2. A need that
/// waits on nobody's decision (a notice) never steps up.
pub(super) fn step(worker: &Worker, need: Need) -> u8 {
    let (first, second) = match need {
        Need::Approval | Need::Question | Need::Failure => (5.0, 15.0),
        Need::Quiet => (10.0, 30.0),
        Need::Notice => return 0,
    };
    match worker.wait_minutes {
        w if w >= second => 2,
        w if w >= first => 1,
        _ => 0,
    }
}

/// The card's border `(width, colour)` at `step`.
pub(super) fn border(need: Need, step: u8) -> (f32, Paint) {
    (1.0 + f32::from(step.min(2)), need_color(need).into())
}

/// The card's fill at `step`.
pub(super) fn fill(need: Need, step: u8) -> Paint {
    if step >= 2 {
        Color::with_alpha(need_color(need), FILL_ALPHA).into()
    } else {
        Role::SurfaceAlt.into()
    }
}
