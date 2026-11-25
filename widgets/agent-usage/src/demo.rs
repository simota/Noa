//! Canned data for `--demo`: three limit sources (one stale, one over pace,
//! one with a scoped bucket and a spend allowance), seven days of limit
//! history and a statistics report with 60 days of synthetic activity, all
//! at a fixed clock (`2026-11-24T12:00:00+09:00`) in a fixed zone (+09:00).
//! Built from the contract types only, so it renders the same with or
//! without the real sources and index.

use agent_usage::history::Series;
use agent_usage::model::{
    Agent, Origin, SourceState, Unix, UsageBucket, UsageSource, UsageSpend, UsageWindow,
};
use agent_usage::stats::types::{
    AgentFilter, Availability, Counters, Counts, Coverage, KeyRow, ModelRow, Period, StatsReport,
    Tokens,
};
use agent_usage::time::{FixedTz, LocalDate, Tz, add_days, day_start, local, parse_rfc3339};

pub const TZ_OFFSET: i32 = 9 * 3600;
/// Days the canned activity reaches back (the "all" period).
const HISTORY_DAYS: i64 = 60;

pub fn now() -> Unix {
    parse_rfc3339("2026-11-24T12:00:00+09:00").expect("valid demo clock")
}

pub fn tz() -> FixedTz {
    FixedTz(TZ_OFFSET)
}

fn window(
    id: &str,
    label: &str,
    used: f64,
    resets_in: i64,
    minutes: i64,
    now: Unix,
) -> UsageWindow {
    UsageWindow {
        id: id.into(),
        label: label.into(),
        used_percent: used,
        resets_at: Some(now + resets_in),
        window_minutes: Some(minutes),
        is_active: false,
    }
}

const H: i64 = 3600;
const D: i64 = 86_400;

/// Claude Code (current, a scoped bucket and a spend allowance, 7d slightly
/// over pace), Codex (current, 7d well over pace: the binding window) and
/// Antigravity (stale for 12 minutes).
pub fn sources(now: Unix) -> Vec<UsageSource> {
    let claude = UsageSource {
        plan: Some("Max".into()),
        windows: vec![
            UsageWindow {
                is_active: true,
                ..window("claude-5h", "5h", 34.0, 3 * H + 8 * 60, 300, now)
            },
            window("claude-7d", "7d", 61.0, 3 * D + 6 * H, 10_080, now),
        ],
        buckets: vec![UsageBucket {
            id: "claude-7d-sonnet".into(),
            label: "Sonnet 7d".into(),
            used_percent: 22.0,
        }],
        spend: Some(UsageSpend {
            label: "Extra usage".into(),
            used_percent: 40.0,
            detail: "$79.71 / $200.00".into(),
        }),
        notes: Vec::new(),
        last_updated: Some(now - 40),
        state: SourceState::Ok,
        origin: Origin::Live,
        ..UsageSource::placeholder(Agent::Claude)
    };
    let codex = UsageSource {
        plan: Some("Pro".into()),
        windows: vec![
            UsageWindow {
                is_active: true,
                ..window("codex-5h", "5h", 12.0, 48 * 60, 300, now)
            },
            window("codex-7d", "7d", 56.0, (4.2 * D as f64) as i64, 10_080, now),
        ],
        notes: vec!["Credits balance: 42".into()],
        last_updated: Some(now - 95),
        state: SourceState::Ok,
        origin: Origin::Live,
        ..UsageSource::placeholder(Agent::Codex)
    };
    let agy = UsageSource {
        windows: vec![
            window("agy-gemini-5h", "Gemini 5h", 72.0, 2 * H, 300, now),
            window("agy-gemini-7d", "Gemini 7d", 40.0, 5 * D, 10_080, now),
            window("agy-claude-gpt-5h", "Claude/GPT 5h", 15.0, 4 * H, 300, now),
        ],
        notes: vec![
            "agy timed out after 12s".into(),
            "Shared with the desktop app and SDK".into(),
        ],
        last_updated: Some(now - 12 * 60),
        state: SourceState::Stale {
            since: now - 12 * 60,
        },
        origin: Origin::Live,
        ..UsageSource::placeholder(Agent::Agy)
    };
    vec![claude, codex, agy]
}

/// A deterministic 0..1 wobble for day `d`.
fn wobble(d: i64, salt: i64) -> f64 {
    ((d * 37 + salt * 11).rem_euclid(17)) as f64 / 17.0
}

/// Seven days of samples every 10 minutes for five windows; Codex misses
/// six hours three days ago (a broken line).
pub fn history(now: Unix) -> Vec<Series> {
    let tz = tz();
    let start = now - 7 * D;
    let five_hour = |peak_salt: i64, end_value: f64, t: Unix| -> f64 {
        let cycle = (t - start).rem_euclid(5 * H) as f64 / (5 * H) as f64;
        let n = (t - start).div_euclid(5 * H);
        let hour = local(t, &tz).hour;
        let active = (9..=23).contains(&hour);
        let peak = 20.0 + 70.0 * wobble(n, peak_salt);
        let v = if active {
            peak * cycle
        } else {
            peak * 0.1 * cycle
        };
        if now - t < 5 * H {
            end_value * ((t - (now - 5 * H)) as f64 / (5 * H) as f64).clamp(0.0, 1.0)
        } else {
            v
        }
    };
    // The current week rises to today's value; the week before it to 85 %.
    let weekly = |end_value: f64, week_start: Unix, t: Unix| -> f64 {
        if t >= week_start {
            end_value * (t - week_start) as f64 / (now - week_start).max(1) as f64
        } else {
            let e = (t - (week_start - 7 * D)) as f64 / (7 * D) as f64;
            85.0 * e.clamp(0.0, 1.0)
        }
    };
    let claude_week = now + 3 * D + 6 * H - 7 * D;
    let codex_week = now + (4.2 * D as f64) as i64 - 7 * D;
    let mut out = vec![
        Series {
            label: "Claude 5h".into(),
            points: Vec::new(),
        },
        Series {
            label: "Claude 7d".into(),
            points: Vec::new(),
        },
        Series {
            label: "Codex 5h".into(),
            points: Vec::new(),
        },
        Series {
            label: "Codex 7d".into(),
            points: Vec::new(),
        },
        Series {
            label: "agy Gemini 5h".into(),
            points: Vec::new(),
        },
    ];
    let gap = (now - 3 * D - 6 * H, now - 3 * D);
    let mut t = start;
    while t <= now {
        let codex_missing = t >= gap.0 && t < gap.1;
        let round = |v: f64| v.round().clamp(0.0, 100.0);
        out[0].points.push((t, round(five_hour(1, 34.0, t))));
        out[1].points.push((t, round(weekly(61.0, claude_week, t))));
        let codex5 = if codex_missing {
            f64::NAN
        } else {
            round(five_hour(2, 12.0, t))
        };
        let codex7 = if codex_missing {
            f64::NAN
        } else {
            round(weekly(56.0, codex_week, t))
        };
        out[2].points.push((t, codex5));
        out[3].points.push((t, codex7));
        // agy went stale 12 minutes ago: no samples since.
        if t <= now - 12 * 60 {
            out[4].points.push((t, round(five_hour(3, 72.0, t))));
        }
        t += 600;
    }
    out
}

/// The first local day and the end of `period` at `now`.
pub fn period_range(period: Period, now: Unix, tz: &dyn Tz) -> (LocalDate, Unix) {
    let today = local(now, tz).date;
    let first = match period {
        Period::Today => today,
        Period::Days7 => add_days(today, -6),
        Period::Days30 => add_days(today, -29),
        Period::All => add_days(today, -(HISTORY_DAYS - 1)),
    };
    (first, now)
}

/// Per-day activity of one agent, `d` days before today.
struct Day {
    tokens: u64,
    sessions: u64,
    turns: u64,
    messages: u64,
    tool_calls: u64,
    active_secs: u64,
    workers: u64,
}

fn day_of(agent: Agent, d: i64, weekday: u8, today_fraction: f64) -> Day {
    let weekend = if weekday >= 5 { 0.35 } else { 1.0 };
    let partial = if d == 0 { today_fraction } else { 1.0 };
    let (salt, tokens, sessions, turns, messages, tools, active, workers) = match agent {
        Agent::Claude => (1, 18.0e6, 7.0, 46.0, 320.0, 210.0, 3.2 * 3600.0, 5.0),
        Agent::Codex => (2, 9.0e6, 3.0, 20.0, 95.0, 150.0, 1.6 * 3600.0, 3.0),
        Agent::Agy => (3, 0.0, 1.4, 6.0, 0.0, 0.0, 1200.0, 0.6),
    };
    let f = (0.55 + 0.9 * wobble(d, salt)) * weekend * partial;
    let r = |v: f64| (v * f).round() as u64;
    Day {
        tokens: r(tokens),
        sessions: r(sessions).max(u64::from(f > 0.05)),
        turns: r(turns),
        messages: r(messages),
        tool_calls: r(tools),
        active_secs: r(active),
        workers: r(workers),
    }
}

/// Token kinds as shares of the total (claude: mostly cache reads).
fn split(agent: Agent, total: u64) -> Tokens {
    let t = total as f64;
    let p = |share: f64| (t * share).round() as u64;
    match agent {
        Agent::Claude => Tokens {
            input: p(0.02),
            output: p(0.04),
            cache_read: p(0.82),
            cache_write: p(0.12),
            cache_write_1h: p(0.05),
            reasoning: p(0.01),
        },
        Agent::Codex => Tokens {
            input: p(0.25),
            output: p(0.10),
            cache_read: p(0.65),
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: p(0.04),
        },
        Agent::Agy => Tokens::default(),
    }
}

/// (model, share of the agent's tokens, USD per million: input, output,
/// cache write 5m, cache write 1h, cache read). Codex models are unpriced.
type Model = (&'static str, f64, Option<[f64; 5]>);

const CLAUDE_MODELS: [Model; 3] = [
    ("claude-opus-5-5", 0.70, Some([4.0, 20.0, 5.0, 8.0, 0.20])),
    (
        "claude-sonnet-5-5",
        0.285,
        Some([2.0, 10.0, 2.5, 4.0, 0.10]),
    ),
    (
        "claude-haiku-5-5",
        0.015,
        Some([0.1, 0.5, 0.125, 0.2, 0.01]),
    ),
];
const CODEX_MODELS: [Model; 3] = [
    ("gpt-6.2-sol", 0.72, None),
    ("gpt-6.1-sol", 0.265, None),
    ("codex-auto-review", 0.015, None),
];

fn cost(tokens: &Tokens, price: [f64; 5]) -> f64 {
    let m = |n: u64| n as f64 / 1e6;
    let write_5m = tokens.cache_write - tokens.cache_write_1h;
    m(tokens.input) * price[0]
        + m(tokens.output) * price[1]
        + m(write_5m) * price[2]
        + m(tokens.cache_write_1h) * price[3]
        + m(tokens.cache_read) * price[4]
}

fn scale(c: &Counts, share: f64) -> Counts {
    let s = |v: u64| (v as f64 * share).round() as u64;
    let t = &c.c.tokens;
    Counts {
        sessions: s(c.sessions),
        c: Counters {
            turns: s(c.c.turns),
            messages: s(c.c.messages),
            tool_calls: s(c.c.tool_calls),
            active_secs: s(c.c.active_secs),
            tokens: Tokens {
                input: s(t.input),
                output: s(t.output),
                cache_read: s(t.cache_read),
                cache_write: s(t.cache_write),
                cache_write_1h: s(t.cache_write_1h),
                reasoning: s(t.reasoning),
            },
        },
        cost_usd: c.cost_usd.map(|v| v * share),
        unpriced_tokens: s(c.unpriced_tokens),
        workers: s(c.workers),
    }
}

fn add_counts(into: &mut Counts, c: &Counts) {
    into.sessions += c.sessions;
    into.c.turns += c.c.turns;
    into.c.messages += c.c.messages;
    into.c.tool_calls += c.c.tool_calls;
    into.c.active_secs += c.c.active_secs;
    into.c.tokens.add(&c.c.tokens);
    into.cost_usd = match (into.cost_usd, c.cost_usd) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    };
    into.unpriced_tokens += c.unpriced_tokens;
    into.workers += c.workers;
}

/// The second is longer than its column: the table cuts it.
pub const PROJECTS: [&str; 18] = [
    "~/work/alpha",
    "~/repos/github.com/example-organization/customer-support-platform/integrations/shared-components",
    "~/work/gamma",
    "~/work/delta",
    "~/work/epsilon",
    "~/work/zeta",
    "~/work/eta",
    "~/work/theta",
    "~/work/iota",
    "~/work/kappa",
    "~/work/lambda",
    "~/work/mu",
    "~/work/nu",
    "~/work/xi",
    "~/work/omicron",
    "~/work/pi",
    "~/work/rho",
    "~/work/sigma",
];

const CLAUDE_TOOLS: [(&str, f64); 8] = [
    ("Read", 0.30),
    ("Bash", 0.25),
    ("Edit", 0.20),
    ("Grep", 0.10),
    ("Write", 0.05),
    ("Glob", 0.05),
    ("Task", 0.03),
    ("WebFetch", 0.02),
];
const CODEX_TOOLS: [(&str, f64); 4] = [
    ("shell", 0.60),
    ("apply_patch", 0.28),
    ("web_search", 0.08),
    ("update_plan", 0.04),
];

/// Relative activity by local hour.
fn hour_profile(h: usize) -> f64 {
    match h {
        9..=11 => 1.0,
        13..=18 => 1.2,
        12 => 0.5,
        19..=23 => 0.6,
        0 | 1 => 0.3,
        _ => 0.02,
    }
}

/// The canned report for `period` and `filter` at `now` — the shape
/// `stats::report` returns.
pub fn report(period: Period, filter: AgentFilter, now: Unix, tz: &dyn Tz) -> StatsReport {
    let today = local(now, tz).date;
    let (first, end) = period_range(period, now, tz);
    let start = day_start(first, tz);
    let now_local = local(now, tz);
    let today_fraction = (f64::from(now_local.hour) + 0.5) / 24.0;
    let mut report = StatsReport::empty((start, end));
    let ndays = {
        let mut n = 0;
        while add_days(first, n) <= today {
            n += 1;
        }
        n
    };
    let agents: Vec<Agent> = Agent::ALL
        .into_iter()
        .filter(|a| filter.matches(*a))
        .collect();

    // Per-day series and per-agent totals.
    let mut days: Vec<(String, [u64; 3])> = Vec::new();
    let mut heat = [[0u64; 24]; 7];
    let mut totals: Vec<Counts> = agents.iter().map(|_| Counts::empty()).collect();
    for i in 0..ndays {
        let date = add_days(first, i);
        let d = ndays - 1 - i; // days before today
        let weekday = local(day_start(date, tz), tz).weekday;
        let mut per = [0u64; 3];
        for (k, agent) in agents.iter().enumerate() {
            let day = day_of(*agent, d, weekday, today_fraction);
            per[agent.index()] = day.tokens;
            let t = &mut totals[k];
            t.sessions += day.sessions;
            t.c.turns += day.turns;
            t.c.messages += day.messages;
            t.c.tool_calls += day.tool_calls;
            t.c.active_secs += day.active_secs;
            t.workers += day.workers;
            t.c.tokens.add(&split(*agent, day.tokens));
            let hours = if d == 0 {
                usize::from(now_local.hour) + 1
            } else {
                24
            };
            // Whole events per hour: rounding the running sum keeps the
            // day's total, so a quiet agent still shows its few turns.
            let weight: f64 = (0..hours).map(hour_profile).sum();
            let events = (day.turns + day.messages) as f64;
            let mut before = 0.0;
            for (h, cell) in heat[usize::from(weekday)][..hours].iter_mut().enumerate() {
                let after = before + events * hour_profile(h) / weight;
                *cell += (after.round() - before.round()) as u64;
                before = after;
            }
        }
        days.push((format!("{}/{}", date.month, date.day), per));
    }
    if period == Period::Today {
        // Today: one bar per hour so far.
        let (_, per) = days.pop().unwrap_or_default();
        let weight: f64 = (0..=usize::from(now_local.hour)).map(hour_profile).sum();
        days = (0..=usize::from(now_local.hour))
            .map(|h| {
                let mut v = [0u64; 3];
                for a in 0..3 {
                    v[a] = (per[a] as f64 * hour_profile(h) / weight).round() as u64;
                }
                (format!("{h}時"), v)
            })
            .collect();
    }
    let mut running = [0u64; 3];
    report.cumulative = days
        .iter()
        .map(|(label, v)| {
            for a in 0..3 {
                running[a] += v[a];
            }
            (label.clone(), running)
        })
        .collect();
    report.days = days;
    report.heatmap = heat;

    // Per model (and the cost of each agent from its models).
    for (k, agent) in agents.iter().enumerate() {
        let models: &[Model] = match agent {
            Agent::Claude => &CLAUDE_MODELS,
            Agent::Codex => &CODEX_MODELS,
            Agent::Agy => &[],
        };
        let base = totals[k].clone();
        let mut agent_cost: Option<f64> = None;
        let mut unpriced = 0;
        for (name, share, price) in models {
            let mut counts = scale(&base, *share);
            counts.sessions = base.sessions;
            counts.workers = 0;
            match price {
                Some(p) => {
                    let c = cost(&counts.c.tokens, *p);
                    counts.cost_usd = Some(c);
                    agent_cost = Some(agent_cost.unwrap_or(0.0) + c);
                }
                None => {
                    counts.cost_usd = None;
                    counts.unpriced_tokens = counts.c.tokens.total();
                    unpriced += counts.unpriced_tokens;
                }
            }
            report.per_model.push(ModelRow {
                agent: *agent,
                model: (*name).to_owned(),
                counts,
            });
        }
        totals[k].cost_usd = agent_cost;
        totals[k].unpriced_tokens = unpriced;
    }
    report
        .per_model
        .sort_by_key(|r| std::cmp::Reverse(r.counts.c.tokens.total()));

    // Per agent and the total.
    for (k, agent) in agents.iter().enumerate() {
        let availability = match agent {
            Agent::Agy => Availability::NoTokens,
            _ => Availability::Full,
        };
        add_counts(&mut report.total, &totals[k]);
        report
            .per_agent
            .push((*agent, totals[k].clone(), availability));
    }

    // Per project: Zipf shares of the selection, top 15 then the rest.
    let weights: Vec<f64> = (0..PROJECTS.len())
        .map(|i| 1.0 / (i as f64 + 1.2))
        .collect();
    let sum: f64 = weights.iter().sum();
    for (name, w) in PROJECTS.iter().zip(&weights).take(15) {
        let mut counts = scale(&report.total, w / sum);
        counts.workers = 0;
        report.per_project.push(KeyRow {
            key: (*name).to_owned(),
            counts,
        });
    }
    report.other_projects = (PROJECTS.len() - 15) as u64;

    // Top tools.
    let mut tools: Vec<(String, u64)> = Vec::new();
    for (k, agent) in agents.iter().enumerate() {
        let list: &[(&str, f64)] = match agent {
            Agent::Claude => &CLAUDE_TOOLS,
            Agent::Codex => &CODEX_TOOLS,
            Agent::Agy => &[],
        };
        for (name, share) in list {
            let n = (totals[k].c.tool_calls as f64 * share).round() as u64;
            tools.push(((*name).to_owned(), n));
        }
    }
    tools.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    report.tools = tools;

    report.coverage = Coverage {
        files_total: 54,
        files_done: 54,
        lines_bad: 2,
        lines_skipped: 1,
        running: false,
    };
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_usage::model::binding_window;

    #[test]
    fn demo_sources_cover_the_states_the_view_draws() {
        let now = now();
        let s = sources(now);
        assert_eq!(
            s.iter().map(|s| s.agent).collect::<Vec<_>>(),
            Agent::ALL.to_vec()
        );
        assert!(s[0].is_current(now) && s[1].is_current(now));
        assert!(matches!(s[2].aged_state(now), SourceState::Stale { .. }));
        assert!(!s[0].buckets.is_empty() && s[0].spend.is_some());
        assert!(s[1].windows[1].pace_delta(now).unwrap() > 0.10);
        let (agent, w) = binding_window(&s, now).unwrap();
        assert_eq!((agent, w.id.as_str()), (Agent::Codex, "codex-7d"));
    }

    #[test]
    fn demo_report_follows_period_and_filter() {
        let now = now();
        let tz = tz();
        let week = report(Period::Days7, AgentFilter::All, now, &tz);
        assert_eq!(week.days.len(), 7);
        assert_eq!(week.per_agent.len(), 3);
        let month = report(Period::Days30, AgentFilter::All, now, &tz);
        assert_eq!(month.days.len(), 30);
        assert!(month.total.c.tokens.total() > week.total.c.tokens.total());
        let today = report(Period::Today, AgentFilter::All, now, &tz);
        assert_eq!(today.days.len(), 13);
        let agy = report(Period::Days7, AgentFilter::One(Agent::Agy), now, &tz);
        assert_eq!(agy.total.c.tokens.total(), 0);
        assert_eq!(agy.per_agent[0].2, Availability::NoTokens);
        let last = week.cumulative.last().unwrap().1;
        let summed: u64 = week.days.iter().map(|(_, v)| v[0]).sum();
        assert_eq!(last[0], summed);
        assert_eq!(history(now).len(), 5);
    }
}
