//! The normalised limits model: every source maps onto these types. Ported
//! from llm-usage `model.rs` with times as `i64` unix seconds.

use serde::{Deserialize, Serialize};

use crate::time::Tz;

pub type Unix = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
    Agy,
}

impl Agent {
    pub const ALL: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::Agy];

    pub fn id(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Agy => "agy",
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Agy => "Antigravity",
        }
    }

    pub fn parse(s: &str) -> Option<Agent> {
        Agent::ALL.into_iter().find(|a| a.id() == s)
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// Over this, a window is called out as burning too fast (severity Critical).
pub const OVER_PACE_THRESHOLD: f64 = 0.10;

/// Slack, as a fraction of the window, for the rounding error of subtracting
/// two ratios. Far below anything a percent-level source can express.
const PACE_EPS: f64 = 1e-9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Normal,
    Caution,
    Warning,
    Critical,
}

impl Severity {
    /// <60, <80, <95, ≥95 (% used).
    pub fn from_used(used_percent: f64) -> Severity {
        if used_percent < 60.0 {
            Severity::Normal
        } else if used_percent < 80.0 {
            Severity::Caution
        } else if used_percent < 95.0 {
            Severity::Warning
        } else {
            Severity::Critical
        }
    }

    pub fn escalated(base: Severity, pace_delta: Option<f64>) -> Severity {
        match pace_delta {
            Some(delta) if delta > OVER_PACE_THRESHOLD + PACE_EPS => base.max(Severity::Critical),
            _ => base,
        }
    }
}

/// One rate-limit window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub id: String,
    /// Short label: "5h", "7d", "Gemini 5h".
    pub label: String,
    /// 0..=100; sources that report *remaining* invert before constructing this.
    pub used_percent: f64,
    pub resets_at: Option<Unix>,
    pub window_minutes: Option<i64>,
    /// The window currently metering, where the source says so (breaks ties).
    pub is_active: bool,
}

impl UsageWindow {
    fn span_secs(&self) -> Option<f64> {
        self.window_minutes
            .filter(|m| *m > 0)
            .map(|m| m as f64 * 60.0)
    }

    /// How far through the window we are, 0..=1; `None` when the window length
    /// or reset time is unknown.
    pub fn elapsed_fraction(&self, now: Unix) -> Option<f64> {
        let resets_at = self.resets_at?;
        let span = self.span_secs()?;
        let start = resets_at as f64 - span;
        Some(((now as f64 - start) / span).clamp(0.0, 1.0))
    }

    /// Positive means consuming faster than the window refills.
    pub fn pace_delta(&self, now: Unix) -> Option<f64> {
        self.elapsed_fraction(now)
            .map(|e| self.used_percent / 100.0 - e)
    }

    /// When the current burn rate reaches 100 %, if before the reset.
    pub fn projected_exhaustion(&self, now: Unix) -> Option<Unix> {
        if self.used_percent >= 100.0 {
            return Some(now);
        }
        let resets_at = self.resets_at?;
        let span = self.span_secs()?;
        if resets_at <= now || self.used_percent <= 0.0 {
            return None;
        }
        let elapsed = self.elapsed_fraction(now).filter(|e| *e >= 0.1)?;
        // On or under pace never runs out; judged on the delta (with slack) so
        // that `57 / 0.57 == 100.00000000000001` cannot predict an exhaustion.
        if self.used_percent / 100.0 - elapsed <= PACE_EPS {
            return None;
        }
        let rate = self.used_percent / elapsed; // percent per whole window
        let at_limit = 100.0 / rate;
        if at_limit <= elapsed {
            return None;
        }
        Some((resets_at as f64 - span + at_limit * span).round() as Unix)
    }

    /// From used %, escalated when over pace.
    pub fn severity(&self, now: Unix) -> Severity {
        Severity::escalated(Severity::from_used(self.used_percent), self.pace_delta(now))
    }

    /// Orders windows by which runs out first: projected exhaustion, then the
    /// active one, then the most used, then id.
    pub fn exhausts_before(&self, other: &UsageWindow, now: Unix) -> bool {
        let a = self.projected_exhaustion(now).unwrap_or(Unix::MAX);
        let b = other.projected_exhaustion(now).unwrap_or(Unix::MAX);
        if a != b {
            return a < b;
        }
        if self.is_active != other.is_active {
            return self.is_active;
        }
        if self.used_percent != other.used_percent {
            return self.used_percent > other.used_percent;
        }
        self.id < other.id
    }
}

/// A finer-grained bucket (e.g. a model-scoped weekly limit).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageBucket {
    pub id: String,
    pub label: String,
    pub used_percent: f64,
}

/// A spending allowance rather than a window: no reset, no pace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageSpend {
    pub label: String,
    pub used_percent: f64,
    /// Detail text, e.g. "$79.71".
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SourceState {
    Ok,
    Stale { since: Unix },
    Error(String),
    Unconfigured,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    Live,
    Log,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageSource {
    pub agent: Agent,
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub buckets: Vec<UsageBucket>,
    pub spend: Option<UsageSpend>,
    pub notes: Vec<String>,
    pub last_updated: Option<Unix>,
    pub state: SourceState,
    /// Seconds a value may sit before it reads as stale.
    pub stale_after: i64,
    pub origin: Origin,
}

impl UsageSource {
    /// Unconfigured, no windows, with the agent's default `stale_after`
    /// (claude 900 s, codex 600 s, agy 900 s).
    pub fn placeholder(agent: Agent) -> Self {
        UsageSource {
            agent,
            plan: None,
            windows: Vec::new(),
            buckets: Vec::new(),
            spend: None,
            notes: Vec::new(),
            last_updated: None,
            state: SourceState::Unconfigured,
            stale_after: match agent {
                Agent::Codex => 600,
                Agent::Claude | Agent::Agy => 900,
            },
            origin: Origin::Live,
        }
    }

    /// `Ok` becomes `Stale` when older than `stale_after` or when a window
    /// reset after the sample was read. Other states are returned as they are.
    pub fn aged_state(&self, now: Unix) -> SourceState {
        let (SourceState::Ok, Some(last)) = (&self.state, self.last_updated) else {
            return self.state.clone();
        };
        let rolled_over = self
            .windows
            .iter()
            .any(|w| w.resets_at.is_some_and(|r| r <= now && last < r));
        if now - last > self.stale_after || rolled_over {
            SourceState::Stale { since: last }
        } else {
            SourceState::Ok
        }
    }

    /// Has windows and a value that is not stale.
    pub fn is_current(&self, now: Unix) -> bool {
        self.last_updated.is_some()
            && !self.windows.is_empty()
            && self.aged_state(now) == SourceState::Ok
    }
}

/// The window that runs out first among current sources; `None` when no
/// current window is on course to run out before its reset.
pub fn binding_window(sources: &[UsageSource], now: Unix) -> Option<(Agent, &UsageWindow)> {
    let mut best: Option<(Agent, &UsageWindow)> = None;
    for source in sources.iter().filter(|s| s.is_current(now)) {
        for w in &source.windows {
            if w.projected_exhaustion(now).is_none() {
                continue;
            }
            if best.is_none_or(|(_, b)| w.exhausts_before(b, now)) {
                best = Some((source.agent, w));
            }
        }
    }
    best
}

/// One recorded window value, a line of the history file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimitSample {
    pub ts: Unix,
    pub agent: Agent,
    pub window_id: String,
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<Unix>,
    pub window_minutes: Option<i64>,
}

pub mod format {
    use super::*;
    use crate::time::{LocalDate, local};

    const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

    /// "11/28(Fri)" in the given zone (English weekday on purpose).
    pub fn day(at: Unix, tz: &dyn Tz) -> String {
        let t = local(at, tz);
        let LocalDate { month, day, .. } = t.date;
        format!("{month}/{day}({})", WEEKDAYS[usize::from(t.weekday)])
    }

    /// "in 48m" / "in 3h08m" / "11/28(Fri)" / "now" / "—".
    pub fn resets_in(at: Option<Unix>, now: Unix, tz: &dyn Tz) -> String {
        let Some(at) = at else { return "—".into() };
        let s = at - now;
        if s <= 0 {
            "now".into()
        } else if s < 3600 {
            format!("in {}m", s / 60)
        } else if s < 86_400 {
            format!("in {}h{:02}m", s / 3600, (s % 3600) / 60)
        } else {
            day(at, tz)
        }
    }

    /// "+27% over pace" from +1 % rounded; `None` when keeping up.
    pub fn over_pace(w: &UsageWindow, now: Unix) -> Option<String> {
        let pct = ((w.pace_delta(now)? + PACE_EPS) * 100.0).round() as i64;
        (pct >= 1).then(|| format!("+{pct}% over pace"))
    }

    /// "maxes out 11/27(Thu) at this pace" — whenever an exhaustion is projected.
    pub fn exhaustion(w: &UsageWindow, now: Unix, tz: &dyn Tz) -> Option<String> {
        let at = w.projected_exhaustion(now)?;
        let when = if at - now < 86_400 {
            resets_in(Some(at), now, tz)
        } else {
            day(at, tz)
        };
        Some(format!("maxes out {when} at this pace"))
    }

    /// "12s ago" / "3m ago" / "2h ago" / "4d ago" / "—".
    pub fn relative(at: Option<Unix>, now: Unix) -> String {
        let Some(at) = at else { return "—".into() };
        let s = (now - at).max(0);
        if s < 60 {
            format!("{s}s ago")
        } else if s < 3600 {
            format!("{}m ago", s / 60)
        } else if s < 86_400 {
            format!("{}h ago", s / 3600)
        } else {
            format!("{}d ago", s / 86_400)
        }
    }

    /// "43%", rounding half away from zero.
    pub fn percent(v: f64) -> String {
        format!("{}%", v.round() as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::FixedTz;

    const NOW: Unix = 1_800_000_000;
    const TZ: FixedTz = FixedTz(9 * 3600);

    fn window(id: &str, used: f64, resets_in: Option<i64>, minutes: Option<i64>) -> UsageWindow {
        UsageWindow {
            id: id.into(),
            label: id.into(),
            used_percent: used,
            resets_at: resets_in.map(|s| NOW + s),
            window_minutes: minutes,
            is_active: false,
        }
    }

    fn source(agent: Agent, windows: Vec<UsageWindow>, last: Unix) -> UsageSource {
        UsageSource {
            windows,
            last_updated: Some(last),
            state: SourceState::Ok,
            ..UsageSource::placeholder(agent)
        }
    }

    #[test]
    fn elapsed_fraction_and_pace_delta() {
        // 5 h window, 2.5 h left: half elapsed.
        let w = window("5h", 70.0, Some(9000), Some(300));
        assert_eq!(w.elapsed_fraction(NOW), Some(0.5));
        assert!((w.pace_delta(NOW).unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(
            window("x", 10.0, None, Some(300)).elapsed_fraction(NOW),
            None
        );
        assert_eq!(window("x", 10.0, Some(60), None).pace_delta(NOW), None);
        assert_eq!(
            window("x", 10.0, Some(60), Some(0)).elapsed_fraction(NOW),
            None
        );
        // Clamped at both ends.
        assert_eq!(
            window("x", 0.0, Some(-10), Some(300)).elapsed_fraction(NOW),
            Some(1.0)
        );
        assert_eq!(
            window("x", 0.0, Some(99_999), Some(300)).elapsed_fraction(NOW),
            Some(0.0)
        );
    }

    #[test]
    fn earlier_exhaustion_wins_across_window_lengths() {
        let short = window("short", 50.0, Some(4 * 3600), Some(300));
        let weekly = window("weekly", 40.0, Some(7 * 86_400 * 9 / 10), Some(10_080));
        assert_eq!(short.projected_exhaustion(NOW), Some(NOW + 3600));
        let e = weekly.projected_exhaustion(NOW).unwrap();
        assert!((e - NOW - (25.2 * 3600.0) as i64).abs() <= 1);
        assert!(short.exhausts_before(&weekly, NOW));
        assert!(!weekly.exhausts_before(&short, NOW));
        // Already full wins, even with no length.
        let full = window("full", 100.0, None, None);
        assert_eq!(full.projected_exhaustion(NOW), Some(NOW));
        assert!(full.exhausts_before(&short, NOW));
        // A window that finishes inside its limit never exhausts.
        assert_eq!(
            window("ok", 10.0, Some(9000), Some(300)).projected_exhaustion(NOW),
            None
        );
    }

    #[test]
    fn rolled_over_window_is_stale() {
        let w = window("5h", 90.0, Some(-1), Some(300));
        let s = source(Agent::Claude, vec![w.clone()], NOW - 2);
        assert_eq!(s.aged_state(NOW), SourceState::Stale { since: NOW - 2 });
        assert!(!s.is_current(NOW));
        assert_eq!(w.projected_exhaustion(NOW), None);
        // Aged out without a rollover.
        let old = source(
            Agent::Codex,
            vec![window("5h", 1.0, Some(3000), Some(300))],
            NOW - 601,
        );
        assert_eq!(old.aged_state(NOW), SourceState::Stale { since: NOW - 601 });
        let fresh = source(
            Agent::Codex,
            vec![window("5h", 1.0, Some(3000), Some(300))],
            NOW - 600,
        );
        assert_eq!(fresh.aged_state(NOW), SourceState::Ok);
        assert!(fresh.is_current(NOW));
        // Non-Ok states are left alone; placeholder has no data.
        let p = UsageSource::placeholder(Agent::Agy);
        assert_eq!(p.aged_state(NOW), SourceState::Unconfigured);
        assert!(!p.is_current(NOW));
    }

    #[test]
    fn over_pace_escalates_to_critical() {
        let w = window("5h", 70.0, Some(9000), Some(300));
        assert_eq!(Severity::from_used(70.0), Severity::Caution);
        assert_eq!(w.severity(NOW), Severity::Critical);
        assert_eq!(
            format::over_pace(&w, NOW).as_deref(),
            Some("+20% over pace")
        );
        // Exactly on the threshold does not escalate; +5 % shows text only.
        let t = window("t", 60.0, Some(9000), Some(300));
        assert_eq!(t.severity(NOW), Severity::Caution);
        let mild = window("m", 55.0, Some(9000), Some(300));
        assert_eq!(mild.severity(NOW), Severity::Normal);
        assert_eq!(
            format::over_pace(&mild, NOW).as_deref(),
            Some("+5% over pace")
        );
        assert_eq!(
            format::over_pace(&window("k", 40.0, Some(9000), Some(300)), NOW),
            None
        );
        assert_eq!(Severity::from_used(79.9), Severity::Caution);
        assert_eq!(Severity::from_used(80.0), Severity::Warning);
        assert_eq!(Severity::from_used(95.0), Severity::Critical);
    }

    #[test]
    fn reset_labels_follow_the_notation() {
        assert_eq!(format::resets_in(None, NOW, &TZ), "—");
        assert_eq!(format::resets_in(Some(NOW - 5), NOW, &TZ), "now");
        assert_eq!(
            format::resets_in(Some(NOW + 48 * 60 + 30), NOW, &TZ),
            "in 48m"
        );
        assert_eq!(
            format::resets_in(Some(NOW + 3 * 3600 + 8 * 60), NOW, &TZ),
            "in 3h08m"
        );
        // 2027-01-15 09:00 UTC = 18:00 JST, a Friday.
        let at = crate::time::parse_rfc3339("2027-01-15T09:00:00Z").unwrap();
        assert_eq!(
            format::resets_in(Some(at), at - 3 * 86_400, &TZ),
            "1/15(Fri)"
        );
        assert_eq!(format::relative(Some(NOW - 12), NOW), "12s ago");
        assert_eq!(format::relative(Some(NOW - 300), NOW), "5m ago");
        assert_eq!(format::relative(Some(NOW - 7200), NOW), "2h ago");
        assert_eq!(format::relative(Some(NOW - 3 * 86_400), NOW), "3d ago");
        assert_eq!(format::relative(None, NOW), "—");
        assert_eq!(format::percent(42.5), "43%");
        assert_eq!(format::percent(0.4), "0%");
        let w = window("7d", 56.0, Some(86_400), Some(10_080));
        assert_eq!(format::exhaustion(&w, NOW, &TZ), None); // not over pace
        let hot = window("5h", 70.0, Some(9000), Some(300));
        assert_eq!(
            format::exhaustion(&hot, NOW, &TZ).as_deref(),
            Some("maxes out in 1h04m at this pace")
        );
        assert_eq!(Agent::parse("codex"), Some(Agent::Codex));
        assert_eq!(Agent::parse("x"), None);
        assert_eq!(Agent::Agy.index(), 2);
        assert_eq!(Agent::Claude.display(), "Claude Code");
    }

    #[test]
    fn binding_window_skips_stale_sources() {
        let hot = window("5h", 70.0, Some(9000), Some(300));
        let calm = window("7d", 5.0, Some(86_400), Some(10_080));
        let current = source(Agent::Codex, vec![calm.clone()], NOW - 10);
        let stale_hot = source(Agent::Claude, vec![hot.clone()], NOW - 5000);
        let sources = [stale_hot.clone(), current.clone()];
        // The calm window is not on course to run out, so nothing binds.
        assert!(binding_window(&sources, NOW).is_none());
        // With the hot one current it binds.
        let fresh_hot = source(Agent::Claude, vec![hot], NOW - 10);
        let sources = [fresh_hot, current];
        assert_eq!(binding_window(&sources, NOW).unwrap().0, Agent::Claude);
        // Nothing current: none.
        assert!(binding_window(&[stale_hot, UsageSource::placeholder(Agent::Agy)], NOW).is_none());
    }

    #[test]
    fn binding_window_ignores_windows_that_will_not_run_out() {
        // Fresh Codex 5h window, half elapsed, 10 % used.
        let calm = window("5h", 10.0, Some(9000), Some(300));
        assert_eq!(calm.projected_exhaustion(NOW), None);
        let sources = [source(Agent::Codex, vec![calm], NOW - 10)];
        assert!(binding_window(&sources, NOW).is_none());
    }

    #[test]
    fn pace_threshold_is_exact_at_the_boundary() {
        // 5400 s of 18000 s elapsed, 40 % used: exactly 10 points ahead.
        let w = window("5h", 40.0, Some(12_600), Some(300));
        assert_eq!(w.severity(NOW), Severity::Normal);
        // 7290 s elapsed, 41 % used: exactly 0.5 points ahead.
        let w = window("5h", 41.0, Some(10_710), Some(300));
        assert_eq!(format::over_pace(&w, NOW).as_deref(), Some("+1% over pace"));
    }

    #[test]
    fn exactly_on_pace_projects_no_exhaustion() {
        // 10260 s of 18000 s elapsed (57 %), 57 % used.
        let w = window("5h", 57.0, Some(7_740), Some(300));
        assert_eq!(w.pace_delta(NOW), Some(0.0));
        assert_eq!(w.projected_exhaustion(NOW), None);
    }

    #[test]
    fn exhaustion_text_follows_the_projection() {
        // 1800 s elapsed, 10.1 % used: runs out 16022 s from now, before the
        // reset at 16200 s, though the lead rounds to 0 %.
        let w = window("5h", 10.1, Some(16_200), Some(300));
        assert_eq!(w.projected_exhaustion(NOW), Some(NOW + 16_022));
        assert_eq!(format::over_pace(&w, NOW), None);
        assert_eq!(
            format::exhaustion(&w, NOW, &TZ).as_deref(),
            Some("maxes out in 4h27m at this pace")
        );
        // Already full with an unknown span.
        let full = window("full", 100.0, None, None);
        assert_eq!(
            format::exhaustion(&full, NOW, &TZ).as_deref(),
            Some("maxes out now at this pace")
        );
    }
}
