//! Calendar days in the local time zone, and what happened on them: the
//! facts the timeline and the calendar place (a run's start and end, a
//! task's close). Tasks have no due dates (spec, Scope), so nothing here is
//! planned, only done.

use tasks::model::types::{Liveness, Millis, ResultFact, RunView, State, Task, TaskRef};

/// A local calendar day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Day {
    pub year: i32,
    /// 1-12.
    pub month: u32,
    /// 1-31.
    pub day: u32,
    /// 0 = Sunday.
    pub weekday: u32,
}

fn tm_of(at: Millis) -> libc::tm {
    let secs = (at / 1000) as libc::time_t;
    // SAFETY: localtime_r writes only the tm it is given.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        tm
    }
}

/// The local day `at` falls on.
pub(super) fn day_of(at: Millis) -> Day {
    let tm = tm_of(at);
    Day {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        weekday: tm.tm_wday as u32,
    }
}

/// Local midnight of `year`-`month`-`day`; `month` and `day` may run over
/// (day 0 is the last of the month before, month 13 January next year),
/// as `mktime` normalises them.
pub(super) fn midnight(year: i32, month: i32, day: i32) -> Millis {
    // SAFETY: mktime reads and normalises the tm it is given.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = year - 1900;
        tm.tm_mon = month - 1;
        tm.tm_mday = day;
        tm.tm_isdst = -1;
        (libc::mktime(&mut tm).max(0) as Millis) * 1000
    }
}

/// Local midnight of the day after the one `at` falls on.
pub(super) fn next_midnight(at: Millis) -> Millis {
    let d = day_of(at);
    midnight(d.year, d.month as i32, d.day as i32 + 1)
}

/// The year and month `offset` months from the one `now` falls in.
pub(super) fn month_from(now: Millis, offset: i32) -> (i32, u32) {
    let d = day_of(now);
    let index = d.year * 12 + d.month as i32 - 1 + offset;
    (index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

/// The 42 days (six weeks, Sunday first) whose grid shows `year`-`month`:
/// each day's local midnight.
pub(super) fn month_grid(year: i32, month: u32) -> [Millis; 42] {
    let first = midnight(year, month as i32, 1);
    let lead = day_of(first).weekday as i32;
    std::array::from_fn(|i| midnight(year, month as i32, 1 - lead + i as i32))
}

/// The `days` local midnights ending with today's, oldest first.
pub(super) fn last_days(now: Millis, days: usize) -> Vec<Millis> {
    let today = day_of(now);
    (0..days)
        .map(|i| {
            midnight(
                today.year,
                today.month as i32,
                today.day as i32 - (days - 1 - i) as i32,
            )
        })
        .collect()
}

/// When a run ended, as far as anything says: its RunClosed, else when its
/// report was written, else now while it is still live; `None` for one
/// that left no end (gone without either).
pub(super) fn run_end(view: &RunView, now: Millis) -> Option<Millis> {
    if let Some(closed) = view.run.closed {
        return Some(closed);
    }
    if let ResultFact::Written { modified, .. } = view.result {
        return Some(modified);
    }
    match view.liveness {
        Liveness::Starting | Liveness::Alive { .. } => Some(now),
        Liveness::Gone => None,
    }
}

/// When a closed task was closed: its source's close time, else its last
/// change (a local task records none).
pub(super) fn closed_at(task: &Task) -> Option<Millis> {
    (task.state == State::Closed).then(|| task.closed.unwrap_or(task.updated))
}

/// One thing that happened, to place on a day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Happened {
    /// A run started on the task, by this agent.
    RunStarted { task: TaskRef, agent: String },
    /// The task was closed.
    Closed { task: TaskRef },
}

impl Happened {
    pub(super) fn task(&self) -> &TaskRef {
        match self {
            Happened::RunStarted { task, .. } | Happened::Closed { task } => task,
        }
    }
}

/// What happened to `tasks` from `from` (inclusive) to `to` (exclusive),
/// oldest first: runs started and tasks closed.
pub(super) fn happened(
    tasks: &[&Task],
    runs: &[RunView],
    from: Millis,
    to: Millis,
) -> Vec<(Millis, Happened)> {
    let shown: Vec<&TaskRef> = tasks.iter().map(|t| &t.r#ref).collect();
    let mut out: Vec<(Millis, Happened)> = runs
        .iter()
        .filter(|v| shown.contains(&&v.run.task))
        .filter(|v| (from..to).contains(&v.run.started))
        .map(|v| {
            (
                v.run.started,
                Happened::RunStarted {
                    task: v.run.task.clone(),
                    agent: v.run.agent.command().to_owned(),
                },
            )
        })
        .chain(tasks.iter().filter_map(|t| {
            let at = closed_at(t)?;
            (from..to).contains(&at).then(|| {
                (
                    at,
                    Happened::Closed {
                        task: t.r#ref.clone(),
                    },
                )
            })
        }))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.task().cmp(b.1.task())));
    out
}

/// `at` as `M/D` in local time.
pub(super) fn month_day(at: Millis) -> String {
    let d = day_of(at);
    format!("{}/{}", d.month, d.day)
}

/// Whether `a` and `b` fall on the same local day.
pub(super) fn same_day(a: Millis, b: Millis) -> bool {
    let (a, b) = (day_of(a), day_of(b));
    (a.year, a.month, a.day) == (b.year, b.month, b.day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;

    const NOW: Millis = 1_795_600_000_000; // 2026-11-25 (UTC)
    /// Days are at most a little over a day apart (a DST change).
    const MAX_DAY_MS: Millis = 86_400_000 + 3_600_000;

    fn start_of_day(at: Millis) -> Millis {
        let d = day_of(at);
        midnight(d.year, d.month as i32, d.day as i32)
    }

    #[test]
    fn the_month_grid_starts_on_the_sunday_on_or_before_the_first_and_runs_six_weeks() {
        for offset in -14..14 {
            let (year, month) = month_from(NOW, offset);
            let grid = month_grid(year, month);
            assert_eq!(day_of(grid[0]).weekday, 0, "{year}-{month}");
            let first = midnight(year, month as i32, 1);
            assert!(grid[..7].contains(&first), "{year}-{month}");
            for pair in grid.windows(2) {
                assert!(pair[1] > pair[0] && pair[1] - pair[0] <= MAX_DAY_MS);
                assert_eq!(start_of_day(pair[1]), pair[1]);
            }
        }
    }

    /// Days and months run over as mktime normalises them, and a date
    /// before the epoch is the epoch (0), never a wrapped huge time.
    #[test]
    fn midnight_normalises_overflow_and_floors_dates_before_the_epoch() {
        // Day 0 is the last of the month before; day 32 of January is
        // February 1st; month 13 is January next year.
        assert_eq!(day_of(midnight(2026, 3, 0)).day, 28);
        assert_eq!(day_of(midnight(2026, 3, 0)).month, 2);
        let d = day_of(midnight(2026, 1, 32));
        assert_eq!((d.month, d.day), (2, 1));
        let d = day_of(midnight(2026, 13, 1));
        assert_eq!((d.year, d.month, d.day), (2027, 1, 1));
        let d = day_of(midnight(2026, 0, 1));
        assert_eq!((d.year, d.month), (2025, 12));
        // Before the epoch: floored, not wrapped.
        assert_eq!(midnight(1960, 1, 1), 0);
        assert!(midnight(1970, 1, 3) > 0);
        assert!(midnight(1970, 1, 3) < 3 * 86_400_000);
        // The epoch itself is a day.
        assert!((1969..=1970).contains(&day_of(0).year));
    }

    #[test]
    fn months_count_across_years() {
        let (y, m) = month_from(NOW, 0);
        let index = |(y, m): (i32, u32)| y * 12 + m as i32;
        assert_eq!(index(month_from(NOW, 13)) - index((y, m)), 13);
        assert_eq!(index(month_from(NOW, -25)) - index((y, m)), -25);
        for offset in -30..30 {
            let (_, m) = month_from(NOW, offset);
            assert!((1..=12).contains(&m));
        }
    }

    #[test]
    fn the_last_days_end_today_one_midnight_apart() {
        let days = last_days(NOW, 14);
        assert_eq!(days.len(), 14);
        assert!(same_day(*days.last().unwrap(), NOW));
        assert!(*days.last().unwrap() <= NOW);
        for pair in days.windows(2) {
            assert!(pair[1] - pair[0] <= MAX_DAY_MS && pair[1] > pair[0]);
        }
    }

    #[test]
    fn what_happened_is_runs_started_and_tasks_closed_in_range_oldest_first() {
        let view = demo::view();
        let tasks: Vec<&Task> = view.tasks.iter().chain(&view.github.tasks).collect();
        let all = happened(&tasks, &view.runs, 0, Millis::MAX);
        assert!(all.windows(2).all(|p| p[0].0 <= p[1].0));
        let started = all
            .iter()
            .filter(|(_, h)| matches!(h, Happened::RunStarted { .. }))
            .count();
        assert_eq!(started, view.runs.len());
        let closed = all
            .iter()
            .filter(|(_, h)| matches!(h, Happened::Closed { .. }))
            .count();
        assert_eq!(
            closed,
            tasks.iter().filter(|t| t.state == State::Closed).count()
        );
        // Nothing outside the range.
        let newest = all.last().unwrap().0;
        assert!(happened(&tasks, &view.runs, newest + 1, Millis::MAX).is_empty());
        // Only the tasks given.
        assert!(happened(&[], &view.runs, 0, Millis::MAX).is_empty());
    }

    #[test]
    fn a_run_ends_at_its_close_its_report_or_now_while_live() {
        let view = demo::view();
        for run in &view.runs {
            let end = run_end(run, view.now);
            match (&run.run.closed, &run.result, &run.liveness) {
                (Some(c), _, _) => assert_eq!(end, Some(*c)),
                (None, ResultFact::Written { modified, .. }, _) => {
                    assert_eq!(end, Some(*modified))
                }
                (None, _, Liveness::Gone) => assert_eq!(end, None),
                (None, _, _) => assert_eq!(end, Some(view.now)),
            }
        }
    }
}
