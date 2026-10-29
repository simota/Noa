//! One idle episode, independent of windowing, terminal locks and GPU state.

use std::time::{Duration, Instant};

use crate::anim::{DUR_EMBER_ENTER, ease_out_cubic};
use crate::split_tree::Rect;

pub(crate) const T_END: f32 = 4.8;
const FRAME: Duration = Duration::from_millis(34);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Waiting,
    Playing { started: Instant },
    Resting,
    Skipped,
}

pub(crate) struct Gate {
    pub enabled_secs: u64,
    pub os_focused: bool,
    pub occluded: bool,
    pub is_main_window: bool,
    pub overlay_active: bool,
    pub ime_composing: bool,
    pub focused_pane_last_output: Instant,
}

impl Gate {
    pub(crate) fn allows_display(&self) -> bool {
        self.enabled_secs > 0
            && self.os_focused
            && !self.occluded
            && self.is_main_window
            && !self.overlay_active
            && !self.ime_composing
    }
}

pub(crate) struct IdleEmber {
    pub state: State,
    last_reset: Instant,
    last_output: Instant,
    blocked: bool,
    next_frame: Option<Instant>,
    sampled_at: Instant,
}

impl IdleEmber {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            state: State::Waiting,
            last_reset: now,
            last_output: now,
            blocked: false,
            next_frame: None,
            sampled_at: now,
        }
    }

    pub(crate) fn visible(&self) -> bool {
        matches!(self.state, State::Playing { .. } | State::Resting)
    }

    pub(crate) fn reset(&mut self, now: Instant) -> bool {
        let was_visible = self.visible();
        self.state = State::Waiting;
        self.last_reset = now;
        self.blocked = false;
        self.next_frame = None;
        was_visible
    }

    pub(crate) fn note_focused_output(&mut self, now: Instant) {
        self.last_output = now;
        if self.visible() {
            self.state = State::Skipped;
            self.next_frame = None;
        }
    }

    pub(crate) fn tick(
        &mut self,
        now: Instant,
        gate: &Gate,
        reduce_motion: impl FnOnce() -> bool,
    ) -> Option<Instant> {
        if gate.enabled_secs == 0 {
            self.state = State::Skipped;
            self.next_frame = None;
            return None;
        }
        let quiet = self.last_output.max(gate.focused_pane_last_output);
        let deadline = self
            .last_reset
            .max(quiet)
            .checked_add(Duration::from_secs(gate.enabled_secs))?;
        if !gate.allows_display() {
            self.blocked = true;
            if self.visible() || now >= deadline {
                self.state = State::Skipped;
            }
            self.next_frame = None;
            return None;
        }
        match self.state {
            State::Waiting if now < deadline => {
                self.blocked = false;
                Some(deadline)
            }
            State::Waiting if self.blocked => {
                self.state = State::Skipped;
                None
            }
            State::Waiting => {
                self.sampled_at = now;
                if reduce_motion() {
                    self.state = State::Resting;
                    None
                } else {
                    self.state = State::Playing { started: now };
                    self.next_frame = now.checked_add(FRAME);
                    self.next_frame
                }
            }
            State::Playing { started } => {
                if now.saturating_duration_since(started).as_secs_f32() >= T_END {
                    self.state = State::Resting;
                    self.next_frame = None;
                } else if self.next_frame.is_none_or(|next| now >= next) {
                    self.sampled_at = now;
                    self.next_frame = now.checked_add(FRAME);
                }
                self.next_frame
            }
            State::Resting | State::Skipped => None,
        }
    }

    pub(crate) fn pose(&self, now: Instant) -> Pose {
        pose(match self.state {
            State::Playing { started } => now
                .min(self.sampled_at)
                .saturating_duration_since(started)
                .as_secs_f32(),
            _ => T_END,
        })
    }
}

pub(crate) fn clear_episode<W>(
    episode: &mut Option<(W, IdleEmber)>,
    deadline: &mut Option<Instant>,
) {
    *episode = None;
    *deadline = None;
}

pub(crate) fn select_host<W: Copy + Eq>(
    episode: &mut Option<(W, IdleEmber)>,
    host: W,
    reset_at: Instant,
) -> Option<W> {
    if episode.as_ref().is_some_and(|(id, _)| *id == host) {
        return None;
    }
    let dismissed = episode
        .as_ref()
        .and_then(|(id, ember)| ember.visible().then_some(*id));
    *episode = Some((host, IdleEmber::new(reset_at)));
    dismissed
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pose {
    pub alpha: f32,
    pub dy: f32,
    pub sx: f32,
    pub sy: f32,
    pub eye_dx: f32,
    pub tilt: f32,
    pub q_scale: f32,
    pub q_alpha: f32,
}

pub(crate) fn pose(t: f32) -> Pose {
    let t = t.clamp(0.0, T_END);
    let smooth = |start: f32, end: f32| {
        let u = ((t - start) / (end - start)).clamp(0.0, 1.0);
        u * u * (3.0 - 2.0 * u)
    };
    let enter = ease_out_cubic(t / DUR_EMBER_ENTER.as_secs_f32());
    let squash = if (0.4..0.6).contains(&t) {
        (1.0 - ((t - 0.5) / 0.1).abs()).max(0.0)
    } else {
        0.0
    };
    let bob = if t >= 0.4 {
        0.04 * (std::f32::consts::TAU * (t - 0.4) / 1.6).sin() * (1.0 - smooth(3.6, T_END))
    } else {
        0.0
    };
    Pose {
        alpha: enter,
        dy: 0.25 * (1.0 - enter) + bob,
        sx: 1.0 + 0.04 * squash,
        sy: 1.0 - 0.05 * squash,
        eye_dx: -0.35 * smooth(1.0, 1.5) + 0.70 * smooth(1.9, 2.5) - 0.35 * smooth(2.5, 2.8),
        tilt: -10.0_f32.to_radians() * smooth(2.9, 3.3),
        q_scale: 1.15 * smooth(3.1, 3.4) - 0.15 * smooth(3.4, 3.55),
        q_alpha: smooth(3.1, 3.25),
    }
}

pub(crate) fn placement(rect: Rect, cell_w: f32, cell_h: f32, scale: f32) -> Option<Rect> {
    if cell_w <= 0.0
        || cell_h <= 0.0
        || (rect.w as f32) < 20.0 * cell_w
        || (rect.h as f32) < 8.0 * cell_h
    {
        return None;
    }
    let h = (3.0 * cell_h / scale).clamp(36.0, 72.0) * scale;
    let w = h * 0.8;
    // The 36 pt floor can outgrow a pane that passes the cell-count check
    // under a small font; never draw past the pane edge.
    if w + cell_w > rect.w as f32 || h + 0.5 * cell_h > rect.h as f32 {
        return None;
    }
    Some(Rect::new(
        (rect.right() as f32 - cell_w - w).round() as u32,
        (rect.bottom() as f32 - 0.5 * cell_h - h).round() as u32,
        w.round().max(1.0) as u32,
        h.round().max(1.0) as u32,
    ))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn reduce_motion() -> bool {
    false
}

#[cfg(target_os = "macos")]
pub(crate) fn reduce_motion() -> bool {
    use objc2::{
        msg_send,
        runtime::{AnyClass, AnyObject},
    };
    // SAFETY: called only by the window-owning event loop; the singleton is nil-checked.
    unsafe {
        let Some(class) = AnyClass::get(c"NSWorkspace") else {
            return false;
        };
        let workspace: *mut AnyObject = msg_send![class, sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let reduced: bool = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        reduced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(now: Instant) -> Gate {
        Gate {
            enabled_secs: 10,
            os_focused: true,
            occluded: false,
            is_main_window: true,
            overlay_active: false,
            ime_composing: false,
            focused_pane_last_output: now,
        }
    }

    #[test]
    fn threshold_uses_input_and_output_deadlines() {
        let now = Instant::now();
        let mut e = IdleEmber::new(now);
        let mut g = gate(now);
        g.focused_pane_last_output = now + Duration::from_secs(3);
        assert_eq!(
            e.tick(now, &g, || false),
            Some(now + Duration::from_secs(13))
        );
        assert_eq!(
            e.tick(now + Duration::from_secs(12), &g, || false),
            Some(now + Duration::from_secs(13))
        );
        e.tick(now + Duration::from_secs(13), &g, || false);
        assert!(matches!(e.state, State::Playing { .. }));
    }

    #[test]
    fn tick_none_after_resting() {
        let now = Instant::now();
        let mut e = IdleEmber::new(now);
        let g = gate(now);
        let start = now + Duration::from_secs(10);
        e.tick(start, &g, || false);
        assert_eq!(
            e.tick(start + Duration::from_millis(4800), &g, || false),
            None
        );
        assert_eq!(e.state, State::Resting);
        assert_eq!(e.tick(start + Duration::from_secs(60), &g, || false), None);
    }

    #[test]
    fn tick_never_returns_past_deadline() {
        let now = Instant::now();
        let mut e = IdleEmber::new(now);
        let g = gate(now);
        for ms in (0..16_000).step_by(34) {
            let t = now + Duration::from_millis(ms);
            if let Some(next) = e.tick(t, &g, || false) {
                assert!(next > t);
                if matches!(e.state, State::Playing { .. }) {
                    assert!(next - t >= FRAME);
                }
            }
        }
    }

    #[test]
    fn reset_visible_episode_returns_to_waiting() {
        let now = Instant::now();
        let mut e = IdleEmber::new(now);
        e.tick(now + Duration::from_secs(10), &gate(now), || false);
        assert!(e.reset(now + Duration::from_secs(11)));
        assert_eq!(e.state, State::Waiting);
        assert!(!e.reset(now + Duration::from_secs(12)));
    }

    #[test]
    fn gate_blocks_while_ime_composing() {
        let now = Instant::now();
        let mut e = IdleEmber::new(now);
        let mut g = gate(now);
        g.ime_composing = true;
        assert_eq!(e.tick(now + Duration::from_secs(10), &g, || false), None);
        assert_eq!(e.state, State::Skipped);
    }

    #[test]
    fn blocked_episode_never_defers_and_focus_loss_dismisses() {
        let now = Instant::now();
        for flag in 0..4 {
            let mut e = IdleEmber::new(now);
            let mut g = gate(now);
            match flag {
                0 => g.os_focused = false,
                1 => g.occluded = true,
                2 => g.overlay_active = true,
                _ => g.is_main_window = false,
            }
            assert_eq!(e.tick(now + Duration::from_secs(10), &g, || false), None);
            assert_eq!(
                e.tick(now + Duration::from_secs(11), &gate(now), || false),
                None
            );
            assert_eq!(e.state, State::Skipped);
            e.reset(now);
            e.tick(now + Duration::from_secs(10), &gate(now), || false);
            assert_eq!(e.tick(now + Duration::from_secs(11), &g, || false), None);
            assert!(!e.visible());
        }
    }

    #[test]
    fn output_in_focused_pane_dismisses() {
        let now = Instant::now();
        for resting in [false, true] {
            let mut e = IdleEmber::new(now);
            let g = gate(now);
            e.tick(now + Duration::from_secs(10), &g, || false);
            if resting {
                e.tick(now + Duration::from_secs(15), &g, || false);
            }
            e.note_focused_output(now + Duration::from_secs(16));
            assert_eq!(e.last_reset, now);
            assert_eq!(e.tick(now + Duration::from_secs(40), &g, || false), None);
            assert_eq!(e.state, State::Skipped);
        }
    }

    #[test]
    fn disabled_in_every_state_and_reduce_motion_rests() {
        let now = Instant::now();
        let mut g = gate(now);
        g.enabled_secs = 0;
        for state in [
            State::Waiting,
            State::Playing { started: now },
            State::Resting,
            State::Skipped,
        ] {
            let mut e = IdleEmber::new(now);
            e.state = state;
            assert_eq!(e.tick(now, &g, || false), None);
            assert!(!e.visible());
        }
        g = gate(now);
        let mut e = IdleEmber::new(now);
        assert_eq!(e.tick(now + Duration::from_secs(10), &g, || true), None);
        assert_eq!(e.state, State::Resting);
    }

    #[test]
    fn reduce_motion_is_sampled_only_at_episode_start() {
        let now = Instant::now();
        let g = gate(now);
        let mut e = IdleEmber::new(now);
        let unexpected = || panic!("reduce motion read outside episode start");
        e.tick(now, &g, unexpected);
        e.tick(now + Duration::from_secs(10), &g, || false);
        e.tick(now + Duration::from_secs(11), &g, unexpected);
        assert!(matches!(e.state, State::Playing { .. }));
        e.tick(now + Duration::from_secs(15), &g, unexpected);
        e.tick(now + Duration::from_secs(16), &g, unexpected);
        e.reset(now);
        e.tick(now + Duration::from_secs(10), &g, || true);
        assert_eq!(e.state, State::Resting);
        e.tick(now + Duration::from_secs(11), &g, unexpected);
        e.reset(now);
        let mut blocked = gate(now);
        blocked.overlay_active = true;
        e.tick(now + Duration::from_secs(10), &blocked, unexpected);
        e.tick(now + Duration::from_secs(11), &g, unexpected);
    }

    #[test]
    fn tick_with_max_secs_no_panic() {
        let now = Instant::now();
        let mut g = gate(now);
        g.enabled_secs = u64::MAX;
        assert_eq!(IdleEmber::new(now).tick(now, &g, || false), None);
    }

    #[test]
    fn reload_to_zero_clears_state_and_deadline() {
        let now = Instant::now();
        let mut e = IdleEmber::new(now);
        let mut deadline = e.tick(now + Duration::from_secs(10), &gate(now), || false);
        let mut episode = Some((1_u32, e));
        assert!(episode.as_ref().unwrap().1.visible());
        assert!(deadline.is_some());
        clear_episode(&mut episode, &mut deadline);
        assert!(episode.is_none());
        assert!(deadline.is_none());
    }

    #[test]
    fn single_ember_across_windows() {
        let now = Instant::now();
        let mut episode = None;
        assert_eq!(select_host(&mut episode, 1_u32, now), None);
        episode
            .as_mut()
            .unwrap()
            .1
            .tick(now + Duration::from_secs(10), &gate(now), || false);
        assert_eq!(select_host(&mut episode, 2, now), Some(1));
        assert_eq!(episode.as_ref().unwrap().0, 2);
        assert_eq!(episode.as_ref().unwrap().1.state, State::Waiting);
        let mut deadline = None;
        clear_episode(&mut episode, &mut deadline);
        assert!(episode.is_none());
    }

    #[test]
    fn redraws_between_frame_deadlines_reuse_pose() {
        let now = Instant::now();
        let start = now + Duration::from_secs(10);
        let mut e = IdleEmber::new(now);
        let g = gate(now);
        e.tick(start, &g, || false);
        let initial = e.pose(start);
        e.tick(start + Duration::from_millis(10), &g, || false);
        assert_eq!(e.pose(start + Duration::from_millis(10)), initial);
        e.tick(start + FRAME, &g, || false);
        assert_ne!(e.pose(start + FRAME), initial);
    }

    #[test]
    fn placement_bottom_right_of_pane_rect() {
        let r = Rect::new(100, 200, 800, 600);
        assert_eq!(
            placement(r, 10.0, 20.0, 1.0),
            Some(Rect::new(842, 730, 48, 60))
        );
        assert!(placement(Rect::new(0, 0, 199, 160), 10.0, 20.0, 1.0).is_none());
        assert!(placement(Rect::new(0, 0, 200, 159), 10.0, 20.0, 1.0).is_none());
        assert_eq!(
            placement(Rect::new(0, 0, 200, 160), 10.0, 20.0, 1.0)
                .unwrap()
                .h,
            60
        );
        // Passes 20×8 cells under a tiny font, but the 36 pt floor won't fit.
        assert!(placement(Rect::new(100, 100, 80, 32), 2.0, 4.0, 1.0).is_none());
    }

    #[test]
    fn pose_has_tilt_bob_and_still_analytic_emote() {
        assert_eq!(pose(0.0).alpha, 0.0);
        assert_eq!(pose(0.4).alpha, 1.0);
        assert!(pose(0.8).dy.abs() > 0.03);
        let p = pose(T_END);
        assert_eq!((p.alpha, p.dy, p.q_alpha, p.q_scale), (1.0, 0.0, 1.0, 1.0));
        assert!((p.tilt.to_degrees() + 10.0).abs() < 0.001);
    }
}

/// Black-box acceptance tests. Expected values come from the acceptance
/// criteria / loop contract text (AC-02..05, AC-09, AC-11, AC-18, pose timeline),
/// not from the implementation.
#[cfg(test)]
mod acceptance_tests {
    use super::*;

    const N: u64 = 10;

    fn open_gate(secs: u64, last_output: Instant) -> Gate {
        Gate {
            enabled_secs: secs,
            os_focused: true,
            occluded: false,
            is_main_window: true,
            overlay_active: false,
            ime_composing: false,
            focused_pane_last_output: last_output,
        }
    }

    fn blocking_variants(last_output: Instant) -> Vec<(&'static str, Gate)> {
        let mut out = Vec::new();
        let mut g = open_gate(N, last_output);
        g.os_focused = false;
        out.push(("unfocused", g));
        let mut g = open_gate(N, last_output);
        g.occluded = true;
        out.push(("occluded", g));
        let mut g = open_gate(N, last_output);
        g.is_main_window = false;
        out.push(("not main window", g));
        let mut g = open_gate(N, last_output);
        g.overlay_active = true;
        out.push(("overlay active", g));
        let mut g = open_gate(N, last_output);
        g.ime_composing = true;
        out.push(("ime composing", g));
        out
    }

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    /// Drive a full play starting at the first deadline; returns
    /// (play_start, frame deadlines, time tick first returned None).
    fn run_play(ember: &mut IdleEmber, gate: &Gate, start: Instant) -> (Vec<Instant>, Instant) {
        let mut now = start;
        let mut deadlines = Vec::new();
        for _ in 0..10_000 {
            match ember.tick(now, gate, || false) {
                Some(d) => {
                    assert!(d > now, "deadline {d:?} not after now {now:?}");
                    deadlines.push(d);
                    now = d;
                }
                None => return (deadlines, now),
            }
        }
        panic!("play never ended");
    }

    #[test]
    fn ac02_zero_never_schedules_or_plays() {
        let t0 = Instant::now();
        let mut ember = IdleEmber::new(t0);
        let mut gates = vec![open_gate(0, t0)];
        for (_, mut g) in blocking_variants(t0) {
            g.enabled_secs = 0;
            gates.push(g);
        }
        for step in 0..(3 * 3600) {
            let now = t0 + Duration::from_secs(step);
            for g in &gates {
                assert_eq!(ember.tick(now, g, || false), None, "step {step}");
                assert_eq!(ember.tick(now, g, || true), None, "step {step}");
                assert!(!matches!(ember.state, State::Playing { .. }));
                assert!(!ember.visible());
            }
            if step % 997 == 0 {
                ember.reset(now);
            }
            if step % 1301 == 0 {
                ember.note_focused_output(now);
            }
        }
    }

    #[test]
    fn ac04_single_rearmed_deadline_from_input_quiet() {
        let t0 = Instant::now();
        let mut ember = IdleEmber::new(t0);
        let gate = open_gate(N, t0);
        let expected = t0 + Duration::from_secs(N);
        for s in [0.0, 0.5, 3.0, 9.0, 9.999] {
            let now = t0 + secs(s);
            // Repeated ticks with identical inputs return the same deadline (no polling).
            let a = ember.tick(now, &gate, || false);
            let b = ember.tick(now, &gate, || false);
            assert_eq!(a, Some(expected), "t={s}");
            assert_eq!(b, Some(expected), "t={s}");
            assert!(expected > now);
            assert_eq!(ember.state, State::Waiting);
        }
    }

    #[test]
    fn ac04_deadline_uses_later_output_stamp() {
        let t0 = Instant::now();
        let mut ember = IdleEmber::new(t0);
        let out = t0 + Duration::from_secs(4);
        let gate = open_gate(N, out);
        let expected = out + Duration::from_secs(N);
        for s in [4.0, 9.0, 10.0, 13.9] {
            let now = t0 + secs(s);
            assert_eq!(ember.tick(now, &gate, || false), Some(expected), "t={s}");
            assert!(
                !ember.visible(),
                "played before output-quiet reached N (t={s})"
            );
        }
        // Input later than output: deadline follows input.
        let mut ember = IdleEmber::new(t0);
        ember.reset(t0 + Duration::from_secs(6));
        assert_eq!(
            ember.tick(t0 + secs(6.5), &gate, || false),
            Some(t0 + Duration::from_secs(16))
        );
    }

    #[test]
    fn ac03_plays_exactly_at_threshold_when_gate_open() {
        let t0 = Instant::now();
        let mut ember = IdleEmber::new(t0);
        let gate = open_gate(N, t0);
        let d = ember.tick(t0, &gate, || false).expect("deadline");
        ember.tick(d - Duration::from_millis(1), &gate, || false);
        assert_eq!(ember.state, State::Waiting);
        ember.tick(d, &gate, || false);
        assert!(
            matches!(ember.state, State::Playing { .. }),
            "{:?}",
            ember.state
        );
    }

    #[test]
    fn ac03_each_blocking_gate_flag_prevents_play() {
        let t0 = Instant::now();
        for (name, gate) in blocking_variants(t0) {
            let mut ember = IdleEmber::new(t0);
            for step in 0..3600 {
                let now = t0 + Duration::from_secs(step);
                let r = ember.tick(now, &gate, || false);
                assert!(
                    !matches!(ember.state, State::Playing { .. } | State::Resting)
                        && !ember.visible(),
                    "{name}: visible at {step}s ({:?})",
                    ember.state
                );
                // AC-10: unfocused/occluded tick schedules nothing.
                if name == "unfocused" || name == "occluded" {
                    assert_eq!(r, None, "{name} at {step}s");
                }
            }
        }
    }

    #[test]
    fn ac05_play_frame_gaps_and_duration_then_none_forever() {
        let t0 = Instant::now();
        let mut ember = IdleEmber::new(t0);
        let gate = open_gate(N, t0);
        let start = ember.tick(t0, &gate, || false).expect("deadline");
        let (deadlines, ended) = run_play(&mut ember, &gate, start);
        assert!(!deadlines.is_empty(), "no play frames scheduled");
        let mut prev = start;
        for d in &deadlines {
            let gap = d.duration_since(prev);
            assert!(gap >= Duration::from_millis(33), "frame gap {gap:?} < 33ms");
            prev = *d;
        }
        let total = ended.duration_since(start);
        assert!(
            total <= Duration::from_secs(5),
            "play lasted {total:?} > 5s"
        );
        assert_eq!(ember.state, State::Resting);
        for step in 0..3600 {
            let now = ended + Duration::from_secs(step);
            assert_eq!(ember.tick(now, &gate, || false), None, "after rest {step}s");
        }
    }

    #[test]
    fn ac09_threshold_passed_while_blocked_skips_episode() {
        let t0 = Instant::now();
        for (name, blocked) in blocking_variants(t0) {
            let mut ember = IdleEmber::new(t0);
            for s in 0..=15 {
                ember.tick(t0 + Duration::from_secs(s), &blocked, || false);
            }
            let open = open_gate(N, t0);
            for s in 15..3600 {
                let now = t0 + Duration::from_secs(s);
                assert_eq!(ember.tick(now, &open, || false), None, "{name} at {s}s");
                assert!(!ember.visible(), "{name}: deferred play at {s}s");
            }
        }
    }

    #[test]
    fn ac03_block_cleared_before_threshold_still_plays() {
        // Positive control for AC-09: blocking that ends before N does not skip.
        let t0 = Instant::now();
        let mut ember = IdleEmber::new(t0);
        let (_, blocked) = blocking_variants(t0).remove(0);
        ember.tick(t0 + Duration::from_secs(2), &blocked, || false);
        let open = open_gate(N, t0);
        let d = ember.tick(t0 + Duration::from_secs(3), &open, || false);
        assert_eq!(d, Some(t0 + Duration::from_secs(N)));
        ember.tick(t0 + Duration::from_secs(N), &open, || false);
        assert!(
            matches!(ember.state, State::Playing { .. }),
            "{:?}",
            ember.state
        );
    }

    #[test]
    fn ac11_reduce_motion_goes_straight_to_rest() {
        let t0 = Instant::now();
        let mut ember = IdleEmber::new(t0);
        let gate = open_gate(N, t0);
        let d = ember.tick(t0, &gate, || true).expect("deadline");
        for step in 0..600 {
            let now = d + Duration::from_millis(17 * step);
            assert_eq!(
                ember.tick(now, &gate, || true),
                None,
                "frame scheduled at step {step}"
            );
            assert_eq!(ember.state, State::Resting);
            assert_eq!(ember.pose(now), pose(T_END));
        }
    }

    #[test]
    fn ac18_output_dismisses_visible_and_no_reshow_in_episode() {
        let t0 = Instant::now();
        // Dismiss while Playing and while Resting.
        for dismiss_at_rest in [false, true] {
            let mut ember = IdleEmber::new(t0);
            let gate = open_gate(N, t0);
            let start = ember.tick(t0, &gate, || false).unwrap();
            ember.tick(start, &gate, || false);
            let mut now = start + Duration::from_millis(500);
            if dismiss_at_rest {
                let (_, ended) = run_play(&mut ember, &gate, start);
                now = ended + Duration::from_secs(1);
            }
            assert!(ember.visible());
            ember.note_focused_output(now);
            assert!(
                !ember.visible(),
                "output did not dismiss (rest={dismiss_at_rest})"
            );
            let gate = open_gate(N, now);
            for s in 0..3600 {
                let t = now + Duration::from_secs(s);
                ember.tick(t, &gate, || false);
                assert!(
                    !ember.visible(),
                    "re-shown {s}s after output (rest={dismiss_at_rest})"
                );
            }
            // A new input-quiet episode can play again.
            let r = now + Duration::from_secs(4000);
            ember.reset(r);
            let d = ember
                .tick(r, &gate, || false)
                .expect("new episode deadline");
            assert_eq!(d, r + Duration::from_secs(N));
            ember.tick(d, &gate, || false);
            assert!(ember.visible(), "new input episode did not play");
        }
    }

    #[test]
    fn pose_timeline_matches_contract() {
        assert_eq!(pose(T_END), pose(T_END + 100.0));
        let rest = pose(T_END);
        let deg = (-10.0_f32).to_radians();
        assert!(
            (rest.tilt - deg).abs() < 1e-3 || (rest.tilt + 10.0).abs() < 1e-3,
            "rest tilt {} is not -10 deg (radians or degrees)",
            rest.tilt
        );
        let start = pose(0.0);
        assert!(start.q_alpha.abs() < 1e-6, "q_alpha(0) = {}", start.q_alpha);
        assert!(
            (rest.q_alpha - 1.0).abs() < 1e-6,
            "q_alpha(rest) = {}",
            rest.q_alpha
        );
        assert!(start.alpha.abs() < 1e-6, "alpha(0) = {}", start.alpha);
        let mut t = 0.4;
        while t <= T_END + 1.0 {
            assert!(
                (pose(t).alpha - 1.0).abs() < 1e-5,
                "alpha({t}) = {}",
                pose(t).alpha
            );
            t += 0.01;
        }
    }
}
