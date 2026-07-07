//! Shared UI animation vocabulary: the easing curve and the duration scale
//! every noa-owned transition draws from, so motion feels uniform across
//! surfaces (quick-terminal slide, overview zoom, palette fade). Pure math —
//! no windowing/GPU/timer state.

use std::time::{Duration, Instant};

/// Fast micro-transition (fades, selection cues). On macOS the modal cards
/// are native AppKit views and don't ride this — the wgpu fallback still does.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub const DUR_FAST: Duration = Duration::from_millis(120);
/// Base transition (spatial movement: zoom, slide-adjacent moves).
pub const DUR_BASE: Duration = Duration::from_millis(150);
/// Slow, screen-scale movement (the quick terminal's full-height slide).
pub const DUR_SLOW: Duration = Duration::from_millis(200);

/// Cubic ease-out (fast start, gentle stop) — the house curve for
/// enter/reveal transitions.
pub fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

/// Linear progress (`0.0..=1.0`) for `elapsed` of `duration`.
pub fn linear_progress(elapsed: Duration, duration: Duration) -> f32 {
    if duration.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
}

/// One in-flight transition: a start instant plus a duration. Progress is
/// sampled against a caller-supplied `now` so pure layout math stays
/// clock-free and testable.
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    start: Instant,
    duration: Duration,
}

impl Tween {
    pub fn new(start: Instant, duration: Duration) -> Self {
        Self { start, duration }
    }

    /// Eased (`ease_out_cubic`) progress in `0.0..=1.0` at `now`.
    pub fn progress(&self, now: Instant) -> f32 {
        ease_out_cubic(linear_progress(
            now.duration_since(self.start),
            self.duration,
        ))
    }

    /// Whether the tween has run its full duration at `now`.
    pub fn done(&self, now: Instant) -> bool {
        now.duration_since(self.start) >= self.duration
    }
}

/// Linear interpolation between `a` and `b`.
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_cubic_hits_endpoints_and_monotone() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!(ease_out_cubic(-1.0) == 0.0 && ease_out_cubic(2.0) == 1.0);
        let mut prev = 0.0;
        for i in 0..=10 {
            let v = ease_out_cubic(i as f32 / 10.0);
            assert!(v >= prev);
            prev = v;
        }
    }

    #[test]
    fn tween_progress_and_done_track_the_clock() {
        let start = Instant::now();
        let tween = Tween::new(start, Duration::from_millis(100));
        assert_eq!(tween.progress(start), 0.0);
        assert!(!tween.done(start));
        let end = start + Duration::from_millis(100);
        assert_eq!(tween.progress(end), 1.0);
        assert!(tween.done(end));
    }

    #[test]
    fn lerp_interpolates() {
        assert_eq!(lerp(10.0, 20.0, 0.0), 10.0);
        assert_eq!(lerp(10.0, 20.0, 0.5), 15.0);
        assert_eq!(lerp(10.0, 20.0, 1.0), 20.0);
    }
}
