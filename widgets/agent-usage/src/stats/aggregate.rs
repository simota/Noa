//! Builds a `StatsReport` for a period and an agent filter from the index.
//!
//! Periods are local calendar days: today = since local midnight; 7d / 30d =
//! today and the 6 / 29 days before; all = everything indexed. Buckets are
//! UTC hours, so a bucket counts when its hour overlaps the period (with a
//! half-hour zone, day boundaries shift by up to 30 min).
//!
//! A report reads only per-hour buckets and the sessions the index derived
//! (hours, active time), never event times: its cost follows the number of
//! hours with data, not of events.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::index::{FileEntry, Index, Kind, codex_parents, session_root};
use super::prices;
use super::types::{
    AgentFilter, Availability, Bucket, Counters, Counts, KeyRow, ModelRow, Period, SkillRow,
    StatsReport,
};
use crate::model::{Agent, Unix};
use crate::time::{LocalDate, Tz, add_days, day_start, local};

/// Projects listed before "その他 N件".
pub(crate) const TOP_PROJECTS: usize = 15;
/// For 全期間 longer than this many days, one slot per ISO week.
pub(crate) const MAX_DAY_SLOTS: usize = 92;
/// The per-model row of an agent's tokens whose model the log does not name.
pub(crate) const UNKNOWN_MODEL: &str = "(unknown)";

fn add_counters(a: &mut Counters, b: &Counters) {
    a.turns += b.turns;
    a.messages += b.messages;
    a.tool_calls += b.tool_calls;
    a.skills += b.skills;
    a.active_secs += b.active_secs;
    a.tokens.add(&b.tokens);
}

#[derive(Default)]
struct SkillAcc {
    uses: u64,
    sessions: BTreeSet<u64>,
    projects: BTreeSet<u32>,
    last: Unix,
}

/// "MM/DD HH:MM" in `tz`.
fn minute_label(at: Unix, tz: &dyn Tz) -> String {
    let t = local(at, tz);
    let minute = (at + i64::from(tz.offset_secs(at))).rem_euclid(3600) / 60;
    format!("{} {:02}:{minute:02}", label(t.date), t.hour)
}

#[derive(Default)]
struct Acc {
    c: Counters,
    cost: f64,
    priced: bool,
    unpriced: u64,
    sessions: u64,
    workers: u64,
}

impl Acc {
    fn add(&mut self, c: &Counters, model: &str) {
        add_counters(&mut self.c, c);
        match prices::cost(model, &c.tokens) {
            Some(usd) => {
                self.cost += usd;
                self.priced = true;
            }
            None => self.unpriced += c.tokens.total(),
        }
    }

    fn counts(&self) -> Counts {
        Counts {
            sessions: self.sessions,
            c: self.c.clone(),
            cost_usd: self.priced.then_some(self.cost),
            unpriced_tokens: self.unpriced,
            workers: self.workers,
        }
    }
}

/// The buckets a file contributes: its own, the Codex `token_count` fallback
/// when it has no usage records, and the active time of its events that name
/// no session.
fn buckets_of(e: &FileEntry) -> impl Iterator<Item = &Bucket> {
    let fallback: &[Bucket] = if e.data.has_records {
        &[]
    } else {
        &e.data.fallback
    };
    e.data
        .buckets
        .iter()
        .chain(fallback)
        .chain(&e.data.unlinked_active)
}

/// Every bucket of the agents `filter` selects: the files' own and the
/// sessions' active time.
fn buckets(index: &Index, filter: AgentFilter) -> impl Iterator<Item = &Bucket> {
    let sessions = index
        .sessions
        .iter()
        .filter(move |((agent, _), _)| filter.matches(*agent))
        .flat_map(|(_, s)| &s.active);
    let files = index.files.values().flat_map(|e| buckets_of(e));
    files
        .filter(move |b| filter.matches(b.agent))
        .chain(sessions)
}

pub(crate) fn file_agent(kind: Kind) -> Option<Agent> {
    match kind {
        Kind::Claude => Some(Agent::Claude),
        Kind::Codex => Some(Agent::Codex),
        Kind::AgyHistory | Kind::AgyDb => Some(Agent::Agy),
        Kind::Journal => None,
    }
}

enum Slots {
    /// Today, per local hour.
    Hours(LocalDate),
    /// One slot per local day, or per ISO week (Monday-first) in `weeks`.
    Days {
        index: HashMap<LocalDate, usize>,
        labels: Vec<String>,
    },
}

fn label(d: LocalDate) -> String {
    format!("{:02}/{:02}", d.month, d.day)
}

impl Slots {
    fn new(
        period: Period,
        first: LocalDate,
        today: LocalDate,
        weekday_of: impl Fn(LocalDate) -> u8,
    ) -> Slots {
        if period == Period::Today {
            return Slots::Hours(today);
        }
        let mut dates = Vec::new();
        let mut d = first;
        while d <= today {
            dates.push(d);
            d = add_days(d, 1);
        }
        let mut index = HashMap::new();
        let mut labels = Vec::new();
        if period == Period::All && dates.len() > MAX_DAY_SLOTS {
            for d in dates {
                let monday = add_days(d, -i64::from(weekday_of(d)));
                if labels.last() != Some(&label(monday)) {
                    labels.push(label(monday));
                }
                index.insert(d, labels.len() - 1);
            }
        } else {
            for (i, d) in dates.into_iter().enumerate() {
                index.insert(d, i);
                labels.push(label(d));
            }
        }
        Slots::Days { index, labels }
    }

    fn len(&self) -> usize {
        match self {
            Slots::Hours(_) => 24,
            Slots::Days { labels, .. } => labels.len(),
        }
    }

    fn labels(&self) -> Vec<String> {
        match self {
            Slots::Hours(_) => (0..24).map(|h| format!("{h:02}")).collect(),
            Slots::Days { labels, .. } => labels.clone(),
        }
    }

    fn slot(&self, at: Unix, tz: &dyn Tz) -> Option<usize> {
        let t = local(at, tz);
        match self {
            Slots::Hours(today) => (t.date == *today).then_some(usize::from(t.hour)),
            Slots::Days { index, .. } => index.get(&t.date).copied(),
        }
    }
}

fn availability(index: &Index, agent: Agent) -> Availability {
    let has_files = index
        .files
        .values()
        .any(|e| file_agent(e.kind) == Some(agent));
    if !index.roots_present[agent.index()] && !has_files {
        return Availability::Missing("no logs found".to_owned());
    }
    match agent {
        Agent::Agy => Availability::NoTokens,
        Agent::Claude | Agent::Codex => Availability::Full,
    }
}

/// The report for `period` and `filter` at `now` in `tz`.
pub fn report(
    index: &Index,
    period: Period,
    filter: AgentFilter,
    now: Unix,
    tz: &dyn Tz,
) -> StatsReport {
    let today = local(now, tz).date;
    let first = match period {
        Period::Today => today,
        Period::Days7 => add_days(today, -6),
        Period::Days30 => add_days(today, -29),
        Period::All => earliest(index, filter, now)
            .map_or(today, |at| local(at, tz).date)
            .min(today),
    };
    let start = day_start(first, tz);
    let in_range = |hour: Unix| hour + 3600 > start && hour <= now;
    let slots = Slots::new(period, first, today, |d| {
        local(day_start(d, tz), tz).weekday
    });

    let mut agents: [Acc; 3] = Default::default();
    let mut models: BTreeMap<(Agent, u32), Acc> = BTreeMap::new();
    let mut projects: BTreeMap<u32, Acc> = BTreeMap::new();
    let mut tools: BTreeMap<u32, u64> = BTreeMap::new();
    let mut skills: BTreeMap<(u32, Agent), SkillAcc> = BTreeMap::new();
    let parents = codex_parents(index);
    let mut days = vec![[0u64; 3]; slots.len()];
    let mut heatmap = [[0u64; 24]; 7];

    for b in buckets(index, filter) {
        if !in_range(b.hour) {
            continue;
        }
        let model = index.string(b.model);
        agents[b.agent.index()].add(&b.c, model);
        models
            .entry((b.agent, b.model))
            .or_default()
            .add(&b.c, model);
        if b.project != 0 {
            projects.entry(b.project).or_default().add(&b.c, model);
        }
        if let Some(slot) = slots.slot(b.hour, tz) {
            days[slot][b.agent.index()] += b.c.tokens.total();
        }
        let t = local(b.hour, tz);
        heatmap[usize::from(t.weekday)][usize::from(t.hour)] += b.c.turns + b.c.messages;
    }
    for e in index.files.values() {
        let agent = file_agent(e.kind);
        if agent.is_some_and(|a| filter.matches(a)) {
            for t in e.data.tools.iter().filter(|t| in_range(t.hour)) {
                *tools.entry(t.name).or_default() += t.count;
            }
        }
        if let Some(agent) = agent.filter(|a| filter.matches(*a)) {
            for k in e.data.skills.iter().filter(|k| in_range(k.hour)) {
                let acc = skills.entry((k.name, agent)).or_default();
                acc.uses += k.count;
                if k.session != 0 {
                    acc.sessions
                        .insert(session_root(&parents, agent, k.session));
                }
                if k.project != 0 {
                    acc.projects.insert(k.project);
                }
                acc.last = acc.last.max(k.last);
            }
        }
        for w in &e.data.workers {
            if filter.matches(w.agent) && w.ts >= start && w.ts <= now {
                agents[w.agent.index()].workers += 1;
            }
        }
    }
    for ((agent, _), s) in &index.sessions {
        if !filter.matches(*agent) || !s.active_between(start, now) {
            continue;
        }
        agents[agent.index()].sessions += 1;
        if s.project != 0 {
            projects.entry(s.project).or_default().sessions += 1;
        }
    }

    let mut total = Acc::default();
    let mut per_agent = Vec::new();
    for agent in Agent::ALL.into_iter().filter(|a| filter.matches(*a)) {
        let a = &agents[agent.index()];
        add_counters(&mut total.c, &a.c);
        total.cost += a.cost;
        total.priced |= a.priced;
        total.unpriced += a.unpriced;
        total.sessions += a.sessions;
        total.workers += a.workers;
        per_agent.push((agent, a.counts(), availability(index, agent)));
    }

    // Model 0 is the log naming no model: a row when it holds tokens, so the
    // rows add up to the total (agy, which has no tokens, gets none).
    let mut per_model: Vec<ModelRow> = models
        .iter()
        .filter(|((_, model), acc)| *model != 0 || acc.c.tokens.total() > 0)
        .map(|((agent, model), acc)| ModelRow {
            agent: *agent,
            model: match *model {
                0 => UNKNOWN_MODEL.to_owned(),
                m => index.string(m).to_owned(),
            },
            counts: acc.counts(),
        })
        .collect();
    per_model.sort_by(|a, b| {
        b.counts
            .c
            .tokens
            .total()
            .cmp(&a.counts.c.tokens.total())
            .then(a.agent.cmp(&b.agent))
            .then(a.model.cmp(&b.model))
    });

    let mut per_project: Vec<KeyRow> = projects
        .iter()
        .map(|(project, acc)| KeyRow {
            key: index.string(*project).to_owned(),
            counts: acc.counts(),
        })
        .collect();
    per_project.sort_by(|a, b| {
        let key = |r: &KeyRow| (r.counts.c.tokens.total(), r.counts.c.turns);
        key(b).cmp(&key(a)).then(a.key.cmp(&b.key))
    });
    let other_projects = per_project.len().saturating_sub(TOP_PROJECTS) as u64;
    per_project.truncate(TOP_PROJECTS);

    let mut tools: Vec<(String, u64)> = tools
        .into_iter()
        .map(|(name, n)| (index.string(name).to_owned(), n))
        .collect();
    tools.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut skills: Vec<SkillRow> = skills
        .into_iter()
        .map(|((name, agent), acc)| SkillRow {
            skill: index.string(name).to_owned(),
            agent,
            uses: acc.uses,
            sessions: acc.sessions.len() as u64,
            projects: acc.projects.len() as u64,
            last: acc.last,
            last_label: minute_label(acc.last, tz),
        })
        .collect();
    skills.sort_by(|a, b| {
        b.uses
            .cmp(&a.uses)
            .then(b.last.cmp(&a.last))
            .then(a.skill.cmp(&b.skill))
            .then(a.agent.cmp(&b.agent))
    });

    let labels = slots.labels();
    let mut running = [0u64; 3];
    let cumulative = labels
        .iter()
        .zip(&days)
        .map(|(l, d)| {
            for (r, v) in running.iter_mut().zip(d) {
                *r += v;
            }
            (l.clone(), running)
        })
        .collect();
    let days = labels.into_iter().zip(days).collect();

    StatsReport {
        range: (start, now),
        total: total.counts(),
        per_agent,
        per_model,
        per_project,
        other_projects,
        tools,
        skills,
        days,
        cumulative,
        heatmap,
        coverage: index.coverage(),
    }
}

/// The first hour with data for the filter, up to `now`.
fn earliest(index: &Index, filter: AgentFilter, now: Unix) -> Option<Unix> {
    let workers = index
        .files
        .values()
        .flat_map(|e| &e.data.workers)
        .filter(|w| filter.matches(w.agent))
        .map(|w| w.ts);
    buckets(index, filter)
        .map(|b| b.hour)
        .chain(workers)
        .filter(|at| *at <= now)
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::index::tests::rederived;
    use crate::stats::index::{Activity, EventAt, FileData, Tail, hour_of};
    use crate::stats::testutil::*;
    use crate::stats::types::SessionSpan;
    use crate::time::{FixedTz, parse_rfc3339};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const JST: FixedTz = FixedTz(9 * 3600);

    fn at(s: &str) -> Unix {
        parse_rfc3339(s).unwrap()
    }

    fn fixture_index() -> Index {
        let t = Tree::fixtures();
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        ix.index().clone()
    }

    /// The fixtures' clock: 2026-11-24 12:00 JST.
    fn now() -> Unix {
        at("2026-11-24T12:00:00+09:00")
    }

    #[test]
    fn periods_are_local_calendar_days() {
        let ix = fixture_index();
        let r = report(&ix, Period::Today, AgentFilter::All, now(), &JST);
        assert_eq!(r.range, (at("2026-11-24T00:00:00+09:00"), now()));
        assert_eq!(r.days.len(), 24);
        assert_eq!(r.days[0].0, "00");
        let r7 = report(&ix, Period::Days7, AgentFilter::All, now(), &JST);
        assert_eq!(r7.range.0, at("2026-11-18T00:00:00+09:00"));
        assert_eq!(r7.days.len(), 7);
        assert_eq!(r7.days[0].0, "11/18");
        assert_eq!(r7.days[6].0, "11/24");
        let r30 = report(&ix, Period::Days30, AgentFilter::All, now(), &JST);
        assert_eq!(r30.range.0, at("2026-10-26T00:00:00+09:00"));
        assert_eq!(r30.days.len(), 30);
        // The Claude main session runs 2026-11-20 00:00–00:07 UTC = 09:00 JST,
        // the subagent 00:10 UTC; the Codex main 2026-11-21 00:00 UTC.
        let claude_day = r7.days.iter().find(|(l, _)| l == "11/20").unwrap().1;
        assert_eq!(claude_day[Agent::Claude.index()], CLAUDE_FIXTURE_TOKENS);
        // The same instant on the other side of local midnight in UTC-10.
        let hst = FixedTz(-10 * 3600);
        let late = report(&ix, Period::Days7, AgentFilter::All, now(), &hst);
        let day = late.days.iter().find(|(l, _)| l == "11/19").unwrap().1;
        assert_eq!(day[Agent::Claude.index()], CLAUDE_FIXTURE_TOKENS);
        // "today" holds only today's activity: the agy turns at 10:00 JST.
        assert_eq!(r.total.c.turns, 2);
        assert_eq!(r.total.c.tokens.total(), 0);
        // All starts at the first data day.
        let all = report(&ix, Period::All, AgentFilter::All, now(), &JST);
        assert_eq!(all.range.0, at("2026-11-20T00:00:00+09:00"));
        assert_eq!(all.total, r30.total);
    }

    #[test]
    fn per_model_and_project_totals_match_fixture() {
        let ix = fixture_index();
        let r = report(&ix, Period::Days30, AgentFilter::All, now(), &JST);
        let model_tokens: u64 = r.per_model.iter().map(|m| m.counts.c.tokens.total()).sum();
        let agent_tokens: u64 = r.per_agent.iter().map(|a| a.1.c.tokens.total()).sum();
        assert_eq!(model_tokens, agent_tokens);
        assert_eq!(agent_tokens, r.total.c.tokens.total());
        assert_eq!(
            r.total.c.tokens.total(),
            CLAUDE_FIXTURE_TOKENS + CODEX_FIXTURE_TOKENS
        );
        let names: Vec<_> = r
            .per_model
            .iter()
            .map(|m| (m.agent, m.model.as_str(), m.counts.c.tokens.total()))
            .collect();
        assert_eq!(
            names,
            vec![
                (Agent::Claude, "claude-sonnet-5-5-20261103", 20 + 30 + 2000),
                (Agent::Codex, "gpt-6.2-sol", 1100 + 540),
                (Agent::Claude, "claude-opus-5-5", 1550 + 16 + 16),
                (Agent::Codex, "gpt-6.2-astra", 1295),
                (Agent::Codex, "gpt-6.1-sol", 410),
                (Agent::Codex, "codex-auto-review", 15),
            ]
        );
        let projects: Vec<_> = r
            .per_project
            .iter()
            .map(|p| (p.key.as_str(), p.counts.c.tokens.total(), p.counts.sessions))
            .collect();
        assert_eq!(
            projects,
            vec![
                ("~/work/alpha", CLAUDE_FIXTURE_TOKENS + 1640 + 410 + 15, 2),
                ("/srv/beta", 1295, 1),
                ("~/work/gamma", 0, 2),
                // Its only prompt names no conversation.
                ("~/work/delta", 0, 0),
            ]
        );
        assert_eq!(r.other_projects, 0);
        assert_eq!(r.tools.first(), Some(&("Bash".to_owned(), 2)));
        // Workers spawned in the period, per agent.
        let workers: Vec<_> = r.per_agent.iter().map(|a| (a.0, a.1.workers)).collect();
        assert_eq!(
            workers,
            vec![(Agent::Claude, 2), (Agent::Codex, 1), (Agent::Agy, 1)]
        );
        // One agent only.
        let codex = report(
            &ix,
            Period::Days30,
            AgentFilter::One(Agent::Codex),
            now(),
            &JST,
        );
        assert_eq!(codex.per_agent.len(), 1);
        assert_eq!(codex.total.c.tokens.total(), CODEX_FIXTURE_TOKENS);
        assert!(codex.per_model.iter().all(|m| m.agent == Agent::Codex));
        assert_eq!(codex.total.sessions, 2);
    }

    #[test]
    fn active_time_caps_gaps_at_five_minutes() {
        let t = Tree::new();
        let log = t.claude_file("-synthetic-gaps", "s.jsonl");
        // Events at 0, 60, 360 (gap 300: counts), 700 (gap 340: does not).
        let mut bytes = claude_turn_and_reply(0, "m1", 1);
        bytes.extend(claude_turn_at(60));
        bytes.extend(claude_turn_at(360));
        write(&log, &bytes);
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        // claude_turn_and_reply puts its reply 5 s after the turn.
        assert_eq!(sum(ix.index()).active_secs, 5 + 55 + 300);
        // Resumed in a later pass: the gap to the last event is kept.
        append(&log, &claude_turn_at(700));
        append(&log, &claude_turn_at(760));
        ix.pass(&mut |_| {});
        assert_eq!(sum(ix.index()).active_secs, 5 + 55 + 300 + 60);
        let r = report(
            ix.index(),
            Period::All,
            AgentFilter::All,
            at("2026-11-21T00:00:00Z"),
            &FixedTz(0),
        );
        assert_eq!(r.total.c.active_secs, 420);
    }

    #[test]
    fn heatmap_cells_by_local_weekday_and_hour() {
        let ix = fixture_index();
        let r = report(&ix, Period::Days30, AgentFilter::All, now(), &JST);
        // 2026-11-20 is a Friday (4). The Claude main session (2 turns, 3
        // messages) and its subagent (1 + 1) run at 09:xx JST; the Codex
        // main thread and its sub-thread on Saturday 09:xx (2 + 2, 1 + 1).
        assert_eq!(r.heatmap[4][9], 2 + 3 + 1 + 1);
        assert_eq!(r.heatmap[5][9], 2 + 2 + 1 + 1);
        let total: u64 = r.heatmap.iter().flatten().sum();
        assert_eq!(total, r.total.c.turns + r.total.c.messages);
        // In UTC the same activity is Friday 00:00.
        let utc = report(
            &ix,
            Period::Days30,
            AgentFilter::One(Agent::Claude),
            now(),
            &FixedTz(0),
        );
        assert_eq!(utc.heatmap[4][0], 2 + 3 + 1 + 1);
        assert_eq!(utc.heatmap.iter().flatten().sum::<u64>(), 7);
    }

    #[test]
    fn cost_is_priced_by_ttl_and_unpriced_counted() {
        let ix = fixture_index();
        let r = report(
            &ix,
            Period::Days30,
            AgentFilter::One(Agent::Claude),
            now(),
            &JST,
        );
        let claude = &r.per_agent[0].1;
        // claude-opus-5-5 (main + subagent): input 100+7+5, output 50+9+11,
        // cache read 1000, cache write 400 of which 300 with the 1 h TTL.
        let opus = (112.0 * 4.0 + 70.0 * 20.0 + 1000.0 * 0.20 + 100.0 * 5.0 + 300.0 * 8.0) / 1e6;
        // claude-sonnet-5-5-20261103: input 20, output 30, cache read 2000.
        let sonnet = (20.0 * 2.0 + 30.0 * 10.0 + 2000.0 * 0.10) / 1e6;
        let usd = claude.cost_usd.unwrap();
        assert!(
            (usd - (opus + sonnet)).abs() < 1e-12,
            "{usd} vs {}",
            opus + sonnet
        );
        assert_eq!(claude.unpriced_tokens, 0);
        let all = report(&ix, Period::Days30, AgentFilter::All, now(), &JST);
        assert_eq!(all.total.unpriced_tokens, CODEX_FIXTURE_TOKENS);
        assert!((all.total.cost_usd.unwrap() - usd).abs() < 1e-12);
        let codex = report(
            &ix,
            Period::Days30,
            AgentFilter::One(Agent::Codex),
            now(),
            &JST,
        );
        assert_eq!(codex.total.cost_usd, None);
        assert_eq!(codex.total.unpriced_tokens, CODEX_FIXTURE_TOKENS);
    }

    #[test]
    fn agy_tokens_are_unavailable_not_zero() {
        let ix = fixture_index();
        let r = report(
            &ix,
            Period::Days30,
            AgentFilter::One(Agent::Agy),
            now(),
            &JST,
        );
        let (agent, counts, avail) = &r.per_agent[0];
        assert_eq!(*agent, Agent::Agy);
        assert_eq!(*avail, Availability::NoTokens);
        assert_eq!(counts.cost_usd, None);
        assert_eq!(counts.c.turns, 4);
        assert_eq!(counts.sessions, 3);
        let all = report(&ix, Period::Days30, AgentFilter::All, now(), &JST);
        let avail: Vec<_> = all.per_agent.iter().map(|a| (a.0, a.2.clone())).collect();
        assert_eq!(
            avail,
            vec![
                (Agent::Claude, Availability::Full),
                (Agent::Codex, Availability::Full),
                (Agent::Agy, Availability::NoTokens),
            ]
        );
        // An agent with no root and no files is missing, not zero.
        let empty = Tree::new();
        let mut eix = empty.indexer();
        eix.pass(&mut |_| {});
        let r = report(eix.index(), Period::Days30, AgentFilter::All, now(), &JST);
        assert!(
            r.per_agent
                .iter()
                .all(|a| matches!(a.2, Availability::Missing(_)))
        );
    }

    #[test]
    fn long_all_period_uses_weeks() {
        let t = Tree::new();
        let log = t.claude_file("-synthetic-long", "s.jsonl");
        // 2026-08-01 (a Saturday) to 2026-11-20: 112 days.
        write(&log, &claude_reply_on("2026-08-01T03:00:00Z", "old", 7));
        append(&log, &claude_reply_on("2026-11-20T03:00:00Z", "new", 5));
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let tz = FixedTz(0);
        let r = report(
            ix.index(),
            Period::All,
            AgentFilter::All,
            at("2026-11-24T12:00:00Z"),
            &tz,
        );
        // Weeks start on Mondays: 07/27 … 11/23 = 18 weeks.
        assert_eq!(r.days.len(), 18);
        assert_eq!(r.days[0].0, "07/27");
        assert_eq!(r.days.last().unwrap().0, "11/23");
        assert_eq!(r.days[0].1[0], 7);
        assert_eq!(r.days[16].1[0], 5); // week of 11/16
        assert_eq!(r.cumulative.last().unwrap().1[0], 12);
        // Up to 92 days stays daily.
        let short = report(
            ix.index(),
            Period::All,
            AgentFilter::All,
            at("2026-10-31T12:00:00Z"),
            &tz,
        );
        assert_eq!(short.days.len(), 92);
    }

    #[test]
    fn active_time_spans_the_files_of_a_session_once() {
        let t = Tree::new();
        let parent = t.claude_file("-synthetic-gaps", "sess-gen.jsonl");
        let sub = t.claude_file("-synthetic-gaps", "sess-gen/subagents/agent-x.jsonl");
        // One event in each file of the session, 2 min apart.
        write(&parent, &claude_turn_at(0));
        write(&sub, &claude_turn_at(120));
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        assert_eq!(ix.index().sessions.len(), 1);
        assert_eq!(sum(ix.index()).active_secs, 120);
        // Overlapping files count once: parent 0, 60, 240 and subagent 90,
        // 120, 180 are 0 … 240 (per file they would add 240 + 90). Another
        // session's event in between is not linked to them.
        append(&parent, &claude_turn_at(60));
        append(&parent, &claude_turn_at(240));
        append(&sub, &claude_turn_at(90));
        append(&sub, &claude_turn_at(180));
        let other = t.claude_file("-synthetic-gaps", "sess-other.jsonl");
        let line = String::from_utf8(claude_turn_at(150)).unwrap();
        write(&other, line.replace("sess-gen", "sess-other").as_bytes());
        ix.pass(&mut |_| {});
        assert_eq!(ix.index().sessions.len(), 2);
        assert_eq!(sum(ix.index()).active_secs, 240);
        let r = report(
            ix.index(),
            Period::All,
            AgentFilter::All,
            at("2026-11-21T00:00:00Z"),
            &FixedTz(0),
        );
        assert_eq!(r.total.c.active_secs, 240);
        assert_eq!(r.total.sessions, 2);
    }

    #[test]
    fn tokens_of_an_unknown_model_have_a_row() {
        let t = Tree::new();
        t.copy_fixture(
            "codex/sessions/2026/11/21/rollout-2026-11-21T09-00-00-00000000-0000-4000-8000-00000000c0de.jsonl",
        );
        // An old rollout: no `turn_context.model`, tokens from `token_count`.
        let id = "00000000-0000-4000-8000-0000000000b1";
        write(
            &t.codex_file(&format!("rollout-2026-11-21T09-10-00-{id}.jsonl")),
            &jsonl(&[
                format!(
                    r#"{{"timestamp":"2026-11-21T00:10:00Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/home/synthetic/work/alpha"}}}}"#
                ),
                r#"{"timestamp":"2026-11-21T00:10:01Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100},"total_token_usage":{"input_tokens":100}}}}"#.to_owned(),
            ]),
        );
        // A Claude prompt before any reply has no model and no tokens: no row.
        write(
            &t.claude_file("-synthetic-gaps", "s.jsonl"),
            &claude_turn_at(0),
        );
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let r = report(
            ix.index(),
            Period::All,
            AgentFilter::All,
            at("2026-11-24T12:00:00Z"),
            &FixedTz(0),
        );
        let unknown: Vec<_> = r
            .per_model
            .iter()
            .filter(|m| m.model == UNKNOWN_MODEL)
            .map(|m| (m.agent, m.counts.c.tokens.total(), m.counts.unpriced_tokens))
            .collect();
        assert_eq!(unknown, vec![(Agent::Codex, 100, 100)]);
        let rows: u64 = r.per_model.iter().map(|m| m.counts.c.tokens.total()).sum();
        assert_eq!(rows, r.total.c.tokens.total());
        assert_eq!(rows, 2065 + 100);
    }

    #[test]
    fn report_reads_no_event_times() {
        let t = Tree::fixtures();
        // A session over two files, and a log whose events name no session.
        write(
            &t.claude_file("-synthetic-gaps", "sess-gen.jsonl"),
            &claude_turn_and_reply(0, "m1", 1),
        );
        write(
            &t.claude_file("-synthetic-gaps", "sess-gen/subagents/agent-x.jsonl"),
            &claude_turn_at(60),
        );
        let unnamed = t.claude_file("-synthetic-gaps", "unnamed.jsonl");
        let lines = String::from_utf8(claude_turn_and_reply(0, "m2", 1)).unwrap();
        write(
            &unnamed,
            lines.replace(r#""sessionId":"sess-gen","#, "").as_bytes(),
        );
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let full = ix.index().clone();
        let unlinked: u64 = full.files[&unnamed]
            .data
            .unlinked_active
            .iter()
            .map(|b| b.c.active_secs)
            .sum();
        assert_eq!(unlinked, 5);
        // Every event time dropped: no report changes.
        let mut bare = full.clone();
        for e in bare.files.values_mut() {
            Arc::make_mut(e).data.activity.clear();
        }
        let filters = [
            AgentFilter::All,
            AgentFilter::One(Agent::Claude),
            AgentFilter::One(Agent::Codex),
            AgentFilter::One(Agent::Agy),
        ];
        for period in [Period::Today, Period::Days7, Period::Days30, Period::All] {
            for filter in filters {
                let r = report(&full, period, filter, now(), &JST);
                assert_eq!(
                    report(&bare, period, filter, now(), &JST),
                    r,
                    "{period:?} {filter:?}"
                );
            }
        }
        let all = report(&full, Period::All, AgentFilter::All, now(), &JST);
        assert_eq!(all.total.c.active_secs, sum(&full).active_secs);
        assert!(all.total.c.active_secs >= 60 + 5);
    }

    #[test]
    fn report_on_a_million_past_event_times_is_fast() {
        // 1000 Claude sessions of 1000 events 2 s apart, one an hour from
        // 2026-10-01: all before today.
        const SESSIONS: u64 = 1000;
        const EVENTS: i64 = 1000;
        let t0 = at("2026-10-01T00:00:00Z");
        let mut index = Index {
            strings: Arc::new(vec![String::new(), "claude-opus-5-5".to_owned()]),
            ..Index::default()
        };
        for key in 1..=SESSIONS {
            let start = t0 + key as i64 * 3600;
            let events: Vec<EventAt> = (0..EVENTS).map(|i| (start + 2 * i, 1, 0)).collect();
            let mut hours: Vec<Unix> = events.iter().map(|e| hour_of(e.0)).collect();
            hours.dedup();
            let bucket = |hour| Bucket {
                hour,
                agent: Agent::Claude,
                model: 1,
                project: 0,
                c: Counters {
                    messages: 1,
                    ..Counters::default()
                },
            };
            let data = FileData {
                buckets: hours.iter().copied().map(bucket).collect(),
                sessions: vec![SessionSpan {
                    agent: Agent::Claude,
                    key,
                    project: 0,
                    hours,
                }],
                activity: vec![Activity { key, events }],
                ..FileData::default()
            };
            let entry = FileEntry {
                kind: Kind::Claude,
                len: 0,
                mtime: 0,
                head_len: 0,
                head_hash: 0,
                offset: 0,
                tail: Tail::None,
                data,
            };
            let path = PathBuf::from(format!("/synthetic/projects/p/{key}.jsonl"));
            index.files.insert(path, Arc::new(entry));
        }
        let index = rederived(&index);
        let events: usize = index
            .files
            .values()
            .map(|e| {
                e.data
                    .activity
                    .iter()
                    .map(|a| a.events.len())
                    .sum::<usize>()
            })
            .sum();
        assert_eq!(events, 1_000_000);
        let tz = FixedTz(0);
        let timed = |period, filter| {
            let began = Instant::now();
            let r = report(&index, period, filter, now(), &tz);
            (r, began.elapsed())
        };
        // Nothing matches: the cost must not follow the event history.
        let (today, took) = timed(Period::Today, AgentFilter::One(Agent::Codex));
        assert_eq!(today.total, Counts::empty());
        assert!(
            took < Duration::from_millis(50),
            "today/codex took {took:?}"
        );
        // Everything matches: still only per-hour buckets and sessions.
        let (all, took) = timed(Period::All, AgentFilter::All);
        assert_eq!(all.total.c.active_secs, SESSIONS * 2 * (EVENTS as u64 - 1));
        assert_eq!(all.total.sessions, SESSIONS);
        assert!(took < Duration::from_millis(50), "all/all took {took:?}");
    }

    #[test]
    fn skills_follow_period_filter_sessions_and_projects() {
        let t = Tree::new();
        let call = |ts: &str, session: &str, cwd: &str, id: &str| {
            format!(
                r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"{session}","cwd":"/home/synthetic/work/{cwd}","message":{{"id":"msg_{id}","model":"claude-opus-5-5","content":[{{"type":"tool_use","id":"{id}","name":"Skill","input":{{"skill":"coding-review"}}}}],"usage":{{"output_tokens":1}}}}}}"#
            )
        };
        write(
            &t.claude_file("-a", "a.jsonl"),
            &jsonl(&[call("2026-11-20T01:00:00Z", "sess-a", "alpha", "tu_a")]),
        );
        // Today 10:30 JST.
        write(
            &t.claude_file("-b", "b.jsonl"),
            &jsonl(&[call("2026-11-24T01:30:00Z", "sess-b", "beta", "tu_b")]),
        );
        const ROOT: &str = "00000000-0000-4000-8000-0000000005a1";
        const SUB: &str = "00000000-0000-4000-8000-0000000005a2";
        for (id, parent, minute) in [(ROOT, None, 0), (SUB, Some(ROOT), 5)] {
            let parent = parent.map_or(String::new(), |p| format!(r#","parent_thread_id":"{p}""#));
            let lines = [
                format!(
                    r#"{{"timestamp":"2026-11-21T00:{minute:02}:00Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/home/synthetic/work/alpha"{parent}}}}}"#
                ),
                format!(
                    r#"{{"timestamp":"2026-11-21T00:{minute:02}:10Z","type":"event_msg","payload":{{"type":"item_completed","turn_id":"{id}","item":{{"type":"CommandExecution","parsed_cmd":[{{"type":"read","path":"~/.codex/skills/coding-review/SKILL.md"}}]}}}}}}"#
                ),
            ];
            write(
                &t.codex_file(&format!("rollout-2026-11-21T09-{minute:02}-00-{id}.jsonl")),
                &jsonl(&lines),
            );
        }
        let mut ix = t.indexer();
        ix.pass(&mut |_| {});
        let ix = ix.index().clone();

        let week = report(&ix, Period::Days7, AgentFilter::All, now(), &JST);
        let rows: Vec<_> = week
            .skills
            .iter()
            .map(|r| (r.agent, r.skill.as_str(), r.uses, r.sessions, r.projects))
            .collect();
        assert_eq!(
            rows,
            vec![
                // Two sessions in two projects; the Codex sub-thread counts
                // toward its root's session.
                (Agent::Claude, "coding-review", 2, 2, 2),
                (Agent::Codex, "coding-review", 2, 1, 1),
            ]
        );
        assert_eq!(week.skills[0].last, at("2026-11-24T01:30:00Z"));
        assert_eq!(week.skills[0].last_label, "11/24 10:30");
        assert_eq!(week.total.c.skills, 4);
        let alpha = week
            .per_project
            .iter()
            .find(|p| p.key == "~/work/alpha")
            .unwrap();
        assert_eq!(alpha.counts.c.skills, 3);

        let today = report(&ix, Period::Today, AgentFilter::All, now(), &JST);
        let rows: Vec<_> = today.skills.iter().map(|r| (r.agent, r.uses)).collect();
        assert_eq!(rows, vec![(Agent::Claude, 1)]);
        let codex = report(
            &ix,
            Period::Days7,
            AgentFilter::One(Agent::Codex),
            now(),
            &JST,
        );
        assert!(codex.skills.iter().all(|r| r.agent == Agent::Codex));
        assert_eq!(codex.skills.len(), 1);
        let agy = report(&ix, Period::All, AgentFilter::One(Agent::Agy), now(), &JST);
        assert!(agy.skills.is_empty());
    }
}
