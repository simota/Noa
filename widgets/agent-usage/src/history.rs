//! Limit sample store: `<data dir>/noa/agent-usage/limits.jsonl`, one
//! `LimitSample` per line, appended with `O_APPEND` one line per `write` (lines
//! stay under 512 bytes, so concurrent writers do not interleave).
//!
//! A sample is written only for a fresh value (`SourceState::Ok`, not aged,
//! `Origin::Live`) and only when 5 min have passed since that window's last
//! sample or its used % moved by at least one point. At open, samples older
//! than 400 days are dropped, rewriting the file (temp file + rename) when more
//! than 10 % of it is old.

use std::collections::{BTreeMap, HashMap};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::model::{Agent, LimitSample, Origin, SourceState, Unix, UsageSource};

/// Minimum seconds between two samples of one window with the same value.
pub const SAMPLE_EVERY: i64 = 300;
/// A change of at least this many points is recorded at once.
pub const SAMPLE_DELTA: f64 = 1.0;
pub const RETENTION_SECS: i64 = 400 * 86_400;

pub struct History {
    path: PathBuf,
    /// Sorted by `ts`.
    samples: Vec<LimitSample>,
    /// The last sample per (agent, window id).
    last: HashMap<(Agent, String), (Unix, f64)>,
}

impl History {
    /// A missing file is an empty history.
    pub fn open(path: PathBuf) -> History {
        History::open_at(path, crate::time::now())
    }

    /// `open` with the clock given (for the 400-day cut-off).
    pub fn open_at(path: PathBuf, now: Unix) -> History {
        let text = std::fs::read(&path).unwrap_or_default();
        let mut all: Vec<LimitSample> = text
            .split(|b| *b == b'\n')
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect();
        let lines = all.len();
        let cutoff = now - RETENTION_SECS;
        all.retain(|s| s.ts >= cutoff);
        let old = lines - all.len();
        all.sort_by_key(|s| s.ts);
        if old * 10 > lines {
            // Best effort: a failed rewrite leaves the old file, read again
            // (and pruned in memory) next time.
            let _ = rewrite(&path, &all);
        }
        let mut last = HashMap::new();
        for s in &all {
            last.insert((s.agent, s.window_id.clone()), (s.ts, s.used_percent));
        }
        History {
            path,
            samples: all,
            last,
        }
    }

    /// Records the source's fresh windows, throttled (L1-4.6).
    pub fn offer(&mut self, source: &UsageSource, now: Unix) -> io::Result<()> {
        if source.origin != Origin::Live || source.aged_state(now) != SourceState::Ok {
            return Ok(());
        }
        let Some(ts) = source.last_updated else {
            return Ok(());
        };
        let mut due = Vec::new();
        for w in &source.windows {
            let key = (source.agent, w.id.clone());
            let is_due = match self.last.get(&key) {
                None => true,
                Some((last_ts, last_used)) => {
                    ts > *last_ts
                        && (ts - last_ts >= SAMPLE_EVERY
                            || (w.used_percent - last_used).abs() >= SAMPLE_DELTA)
                }
            };
            if !is_due {
                continue;
            }
            let sample = LimitSample {
                ts,
                agent: source.agent,
                window_id: w.id.clone(),
                label: w.label.clone(),
                used_percent: w.used_percent,
                resets_at: w.resets_at,
                window_minutes: w.window_minutes,
            };
            let mut line = serde_json::to_vec(&sample).map_err(io::Error::other)?;
            line.push(b'\n');
            due.push((key, sample, line));
        }
        if due.is_empty() {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        // A sample is saved (and throttles the next) only once its line is
        // written: after a failure it and the windows after it stay due.
        for (key, sample, line) in due {
            // One `write` per line: O_APPEND puts each at the end atomically.
            let n = f.write(&line)?;
            if n != line.len() {
                f.write_all(&line[n..])?;
            }
            self.last.insert(key, (ts, sample.used_percent));
            let at = self.samples.partition_point(|s| s.ts <= ts);
            self.samples.insert(at, sample);
        }
        Ok(())
    }

    pub fn samples(&self, since: Unix) -> &[LimitSample] {
        let start = self.samples.partition_point(|s| s.ts < since);
        &self.samples[start..]
    }
}

fn rewrite(path: &std::path::Path, samples: &[LimitSample]) -> io::Result<()> {
    let mut out = Vec::new();
    for s in samples {
        serde_json::to_writer(&mut out, s).map_err(io::Error::other)?;
        out.push(b'\n');
    }
    let tmp = path.with_extension(format!("jsonl.tmp-{}", std::process::id()));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(&out)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    /// "Claude 5h", "Codex 7d", "agy Gemini 5h".
    pub label: String,
    /// (time, used %); a `NAN` point breaks the line.
    pub points: Vec<(Unix, f64)>,
}

fn short(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "Claude",
        Agent::Codex => "Codex",
        Agent::Agy => "agy",
    }
}

/// One series per window over `range` (inclusive), ordered by agent then
/// window id; a gap over `max_gap` seconds between two samples breaks the
/// line with a `NAN` point between them.
pub fn series(samples: &[LimitSample], range: (Unix, Unix), max_gap: i64) -> Vec<Series> {
    // (agent, window id) → (latest label, points).
    type Window = (String, Vec<(Unix, f64)>);
    let mut by: BTreeMap<(Agent, &str), Window> = BTreeMap::new();
    for s in samples
        .iter()
        .filter(|s| s.ts >= range.0 && s.ts <= range.1)
    {
        let slot = by
            .entry((s.agent, s.window_id.as_str()))
            .or_insert_with(|| (String::new(), Vec::new()));
        slot.0 = format!("{} {}", short(s.agent), s.label);
        slot.1.push((s.ts, s.used_percent));
    }
    by.into_values()
        .map(|(label, mut pts)| {
            pts.sort_by_key(|p| p.0);
            let mut points = Vec::with_capacity(pts.len());
            for p in pts {
                if let Some(&(prev, _)) = points.last()
                    && p.0 - prev > max_gap
                {
                    points.push((prev + (p.0 - prev) / 2, f64::NAN));
                }
                points.push(p);
            }
            Series { label, points }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{UsageSource, UsageWindow};

    const T0: Unix = 1_795_489_200; // 2026-11-24T12:00:00+09:00

    fn source(agent: Agent, at: Unix, used: &[(&str, f64)]) -> UsageSource {
        let mut s = UsageSource::placeholder(agent);
        s.state = SourceState::Ok;
        s.last_updated = Some(at);
        s.windows = used
            .iter()
            .map(|(id, u)| UsageWindow {
                id: (*id).to_owned(),
                label: id.rsplit('-').next().unwrap().to_owned(),
                used_percent: *u,
                resets_at: Some(at + 3600),
                window_minutes: Some(300),
                is_active: false,
            })
            .collect();
        s
    }

    fn file_lines(path: &std::path::Path) -> Vec<LimitSample> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn fresh_samples_are_throttled_to_five_minutes_or_one_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("own/limits.jsonl");
        let mut h = History::open_at(path.clone(), T0);
        assert!(h.samples(0).is_empty());
        let offer = |h: &mut History, at: Unix, used: f64| {
            h.offer(&source(Agent::Claude, at, &[("claude-5h", used)]), at)
                .unwrap()
        };
        offer(&mut h, T0, 10.0); // first: written
        offer(&mut h, T0, 10.0); // the same sample again
        offer(&mut h, T0 + 60, 10.5); // < 5 min, < 1 point
        offer(&mut h, T0 + 120, 11.0); // 1 point: written
        offer(&mut h, T0 + 240, 11.2); // < 5 min since the last sample
        offer(&mut h, T0 + 420, 11.2); // 5 min: written
        let ts: Vec<_> = file_lines(&path).iter().map(|s| s.ts).collect();
        assert_eq!(ts, vec![T0, T0 + 120, T0 + 420]);
        assert_eq!(h.samples(T0 + 1).len(), 2);
        // Windows are throttled independently; a reopened store keeps the
        // last sample per window.
        let mut h = History::open_at(path.clone(), T0 + 480);
        h.offer(
            &source(
                Agent::Claude,
                T0 + 480,
                &[("claude-5h", 11.3), ("claude-7d", 40.0)],
            ),
            T0 + 480,
        )
        .unwrap();
        let last = file_lines(&path).pop().unwrap();
        assert_eq!((last.window_id.as_str(), last.ts), ("claude-7d", T0 + 480));
        assert_eq!(file_lines(&path).len(), 4);
        // Every line is well under the 512-byte atomic-append bound.
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .all(|l| l.len() < 512)
        );
    }

    #[test]
    fn stale_and_log_values_are_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limits.jsonl");
        let mut h = History::open_at(path.clone(), T0);
        let mut stale = source(Agent::Codex, T0, &[("codex-5h", 50.0)]);
        stale.state = SourceState::Stale { since: T0 - 900 };
        h.offer(&stale, T0).unwrap();
        let mut log = source(Agent::Codex, T0, &[("codex-5h", 50.0)]);
        log.origin = Origin::Log;
        h.offer(&log, T0).unwrap();
        // Ok but aged past `stale_after` (600 s for Codex).
        let old = source(Agent::Codex, T0 - 700, &[("codex-5h", 50.0)]);
        h.offer(&old, T0).unwrap();
        let mut failed = source(Agent::Codex, T0, &[("codex-5h", 50.0)]);
        failed.state = SourceState::Error("synthetic".into());
        h.offer(&failed, T0).unwrap();
        assert!(!path.exists());
        assert!(h.samples(0).is_empty());
        h.offer(&source(Agent::Codex, T0, &[("codex-5h", 50.0)]), T0)
            .unwrap();
        assert_eq!(file_lines(&path).len(), 1);
    }

    #[test]
    fn old_samples_are_pruned_by_rename() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limits.jsonl");
        let sample = |ts: Unix| LimitSample {
            ts,
            agent: Agent::Agy,
            window_id: "agy-gemini-5h".into(),
            label: "Gemini 5h".into(),
            used_percent: 1.0,
            resets_at: None,
            window_minutes: Some(300),
        };
        let write_file = |old: usize, new: usize| {
            let mut text = String::new();
            for i in 0..old {
                text +=
                    &serde_json::to_string(&sample(T0 - RETENTION_SECS - 10 - i as i64)).unwrap();
                text.push('\n');
            }
            for i in 0..new {
                text += &serde_json::to_string(&sample(T0 - 100 + i as i64)).unwrap();
                text.push('\n');
            }
            text += "not json\n";
            std::fs::write(&path, text).unwrap();
            std::fs::metadata(&path).unwrap().ino()
        };
        // 1 old in 11: under 10 %, the file is left alone (but the old sample
        // is not served).
        let ino = write_file(1, 10);
        let h = History::open_at(path.clone(), T0);
        assert_eq!(h.samples(0).len(), 10);
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), ino);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 12);
        // 3 old in 13: rewritten through a new file.
        let ino = write_file(3, 10);
        let h = History::open_at(path.clone(), T0);
        assert_eq!(h.samples(0).len(), 10);
        assert_ne!(std::fs::metadata(&path).unwrap().ino(), ino);
        let kept = file_lines(&path);
        assert_eq!(kept.len(), 10);
        assert!(kept.iter().all(|s| s.ts >= T0 - RETENTION_SECS));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("limits.jsonl")]);
    }

    #[test]
    fn series_break_at_gaps() {
        let s = |ts: Unix, agent: Agent, id: &str, label: &str, used: f64| LimitSample {
            ts,
            agent,
            window_id: id.into(),
            label: label.into(),
            used_percent: used,
            resets_at: None,
            window_minutes: None,
        };
        let samples = vec![
            s(T0 - 10_000, Agent::Claude, "claude-5h", "5h", 1.0), // before the range
            s(T0, Agent::Codex, "codex-7d", "7d", 20.0),
            s(T0, Agent::Claude, "claude-5h", "5h", 10.0),
            s(T0 + 300, Agent::Claude, "claude-5h", "5h", 12.0),
            s(T0 + 2000, Agent::Claude, "claude-5h", "5h", 3.0), // gap 1700 > 600
            s(T0 + 2300, Agent::Codex, "codex-7d", "7d", 21.0),
        ];
        let out = series(&samples, (T0 - 60, T0 + 3000), 600);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].label, "Claude 5h");
        assert_eq!(out[1].label, "Codex 7d");
        let claude = &out[0].points;
        assert_eq!(claude.len(), 4);
        assert_eq!(claude[0], (T0, 10.0));
        assert_eq!(claude[1], (T0 + 300, 12.0));
        assert!(claude[2].1.is_nan() && claude[2].0 > T0 + 300 && claude[2].0 < T0 + 2000);
        assert_eq!(claude[3], (T0 + 2000, 3.0));
        let codex = &out[1].points;
        assert_eq!(codex.len(), 3);
        assert!(codex[1].1.is_nan());
        assert!(series(&samples, (T0 + 5000, T0 + 6000), 600).is_empty());
    }

    #[test]
    fn concurrent_appends_do_not_interleave() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limits.jsonl");
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut h = History::open_at(path, T0);
                    let id = format!("claude-window-with-a-long-identifier-{t}");
                    for i in 0..200 {
                        let at = T0 + i * SAMPLE_EVERY;
                        h.offer(
                            &source(Agent::Claude, at, &[(id.as_str(), i as f64 / 4.0)]),
                            at,
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let lines = file_lines(&path);
        assert_eq!(lines.len(), 8 * 200);
        for t in 0..8 {
            let id = format!("claude-window-with-a-long-identifier-{t}");
            let ts: Vec<_> = lines
                .iter()
                .filter(|s| s.window_id == id)
                .map(|s| s.ts)
                .collect();
            assert_eq!(ts.len(), 200);
            assert!(ts.windows(2).all(|w| w[0] < w[1]));
        }
    }

    #[test]
    fn failed_save_is_not_marked_saved() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the directory should be: the history cannot be opened.
        let blocker = dir.path().join("own");
        std::fs::write(&blocker, b"").unwrap();
        let path = blocker.join("limits.jsonl");
        let mut h = History::open_at(path.clone(), T0);
        let src = source(
            Agent::Claude,
            T0,
            &[("claude-5h", 10.0), ("claude-7d", 40.0)],
        );
        assert!(h.offer(&src, T0).is_err());
        assert!(h.samples(0).is_empty());
        // Once the path works again the same samples are written.
        std::fs::remove_file(&blocker).unwrap();
        h.offer(&src, T0).unwrap();
        assert_eq!(file_lines(&path).len(), 2);
        assert_eq!(h.samples(0).len(), 2);
    }

    /// `/dev/full` opens, but every write fails.
    #[cfg(target_os = "linux")]
    #[test]
    fn failed_write_is_not_marked_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limits.jsonl");
        let mut h = History::open_at(path.clone(), T0);
        h.path = PathBuf::from("/dev/full");
        let src = source(Agent::Codex, T0, &[("codex-5h", 10.0), ("codex-7d", 40.0)]);
        assert!(h.offer(&src, T0).is_err());
        assert!(h.samples(0).is_empty());
        h.path = path.clone();
        h.offer(&src, T0).unwrap();
        assert_eq!(file_lines(&path).len(), 2);
        assert_eq!(h.samples(0).len(), 2);
    }
}
