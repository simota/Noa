//! Antigravity limits through `agy`'s read-only `/usage` print command. The
//! CLI owns its login; no credential is read here. Versions before 1.1.11 may
//! send `/usage` to the model as a prompt, so the version is checked first,
//! every cycle (a downgrade is caught too).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use super::cli::{self, Cancel, Resolution, RunFailure};
use super::{Clock, Ctl, Link, POLL_SECS, Slot, Spawner, unavailable};
use crate::model::{Agent, Origin, SourceState, UsageSource, UsageWindow};
use crate::time::parse_rfc3339;

const SHARED_NOTE: &str = "Shared with the desktop app and SDK";

#[derive(Deserialize)]
struct Bucket {
    id: String,
    window: String,
    remaining_fraction: Option<f64>,
    reset_time: Option<String>,
}

#[derive(Deserialize)]
struct Group {
    name: String,
    buckets: Vec<Bucket>,
}

#[derive(Deserialize)]
struct Summary {
    groups: Vec<Group>,
}

#[derive(Deserialize)]
struct Command {
    name: String,
    data: Summary,
}

#[derive(Deserialize)]
struct Envelope {
    status: String,
    num_turns: i64,
    command: Command,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Missing,
    Outdated,
    Failed,
    TimedOut,
    Unexpected,
}

impl Failure {
    pub fn reason(self) -> &'static str {
        match self {
            Failure::Missing => "agy not found (install Antigravity CLI)",
            Failure::Outdated => "Update Antigravity CLI (agy 1.1.11+ required)",
            Failure::Failed => "Antigravity unavailable (check agy /usage and login)",
            Failure::TimedOut => "Antigravity usage request timed out",
            Failure::Unexpected => "Unexpected Antigravity response (update agy)",
        }
    }
}

pub struct Deps {
    pub resolve: Arc<dyn Fn() -> Option<Resolution> + Send + Sync>,
    pub now: Clock,
    pub cancel: Cancel,
    /// `agy --version` (3 s).
    pub version_timeout: Duration,
    /// `agy -p /usage` (12 s; agy's own `--print-timeout` is 10 s).
    pub usage_timeout: Duration,
}

impl Deps {
    pub fn system(now: Clock, cancel: Cancel, resolver: Arc<cli::Resolver>) -> Deps {
        Deps {
            resolve: Arc::new(move || resolver.resolve("agy")),
            now,
            cancel,
            version_timeout: Duration::from_secs(3),
            usage_timeout: Duration::from_secs(12),
        }
    }
}

/// The installed version from `agy --version`: the first line is
/// `X.Y.Z`, `vX.Y.Z` or either after the word `agy`; anything after that
/// token (an "update available: …" notice) is ignored. `None` for any other
/// shape.
pub fn installed_version(output: &str) -> Option<(u64, u64, u64)> {
    let mut words = output.trim_start().lines().next()?.split_whitespace();
    let mut word = words.next()?;
    if word == "agy" {
        word = words.next()?;
    }
    let mut parts = word.strip_prefix('v').unwrap_or(word).split('.');
    let mut number = || {
        parts
            .next()
            .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))?
            .parse::<u64>()
            .ok()
    };
    let version = (number()?, number()?, number()?);
    parts.next().is_none().then_some(version)
}

/// `1.1.11` or later. A version it cannot read is refused.
pub fn supports_usage(output: &str) -> bool {
    installed_version(output).is_some_and(|v| v >= (1, 1, 11))
}

fn short_group(name: &str) -> String {
    if name.is_empty() {
        return "—".into();
    }
    let name = name
        .strip_suffix(" Models")
        .or_else(|| name.strip_suffix(" models"))
        .unwrap_or(name);
    name.replace(" and ", "/")
}

/// Validates the `/usage` envelope and normalises it.
pub fn parse_usage(json: &[u8], now: crate::model::Unix) -> Result<UsageSource, Failure> {
    let env: Envelope = serde_json::from_slice(json).map_err(|_| Failure::Unexpected)?;
    let valid = env.status == "SUCCESS"
        && env.num_turns == 0
        && env.command.name == "usage"
        && env.command.data.groups.iter().all(|g| {
            g.buckets.iter().all(|b| {
                !b.id.is_empty()
                    && b.remaining_fraction
                        .is_none_or(|f| f.is_finite() && (0.0..=1.0).contains(&f))
            })
        });
    if !valid {
        return Err(Failure::Unexpected);
    }
    let windows = env
        .command
        .data
        .groups
        .iter()
        .flat_map(|g| {
            let group = short_group(&g.name);
            let mut ws: Vec<UsageWindow> = g
                .buckets
                .iter()
                .filter_map(|b| {
                    let remaining = b.remaining_fraction?;
                    let (label, minutes) = match b.window.as_str() {
                        "5h" => ("5h", Some(300)),
                        "weekly" => ("7d", Some(10_080)),
                        "" => ("—", None),
                        other => (other, None),
                    };
                    Some(UsageWindow {
                        id: b.id.clone(),
                        label: format!("{group} {label}"),
                        used_percent: (1.0 - remaining) * 100.0,
                        resets_at: b.reset_time.as_deref().and_then(parse_rfc3339),
                        window_minutes: minutes,
                        is_active: false,
                    })
                })
                .collect();
            ws.sort_by_key(|w| w.window_minutes.unwrap_or(i64::MAX));
            ws
        })
        .collect();
    Ok(UsageSource {
        windows,
        notes: vec![SHARED_NOTE.into()],
        last_updated: Some(now),
        state: SourceState::Ok,
        origin: Origin::Live,
        ..UsageSource::placeholder(Agent::Agy)
    })
}

fn read_usage(deps: &Deps) -> Result<UsageSource, Failure> {
    let r = (deps.resolve)().ok_or(Failure::Missing)?;
    let run = |args: &[&str], timeout| {
        cli::run(
            Path::new(&r.path),
            args,
            Some(&r.env),
            None,
            timeout,
            &[0],
            Some(&deps.cancel),
        )
        .map_err(|e| match e {
            RunFailure::TimedOut => Failure::TimedOut,
            _ => Failure::Failed,
        })
    };
    let version = run(&["--version"], deps.version_timeout)?;
    if !supports_usage(&String::from_utf8_lossy(&version)) {
        return Err(Failure::Outdated);
    }
    let json = run(
        &[
            "-p",
            "/usage",
            "--output-format",
            "json",
            "--print-timeout",
            "10s",
        ],
        deps.usage_timeout,
    )?;
    parse_usage(&json, (deps.now)())
}

pub(crate) fn spawn(deps: Deps, spawner: &Spawner) -> Slot {
    spawner.spawn(Agent::Agy, move |link: Link<()>| {
        let mut last_good: Option<UsageSource> = None;
        loop {
            let result = read_usage(&deps);
            if link.stopped() || link.absorb_refreshes() {
                return;
            }
            let source = match result {
                Ok(s) => {
                    last_good = Some(s.clone());
                    s
                }
                Err(f) => unavailable(
                    Agent::Agy,
                    last_good.as_ref(),
                    f.reason(),
                    f == Failure::Missing,
                ),
            };
            link.publish(source);
            let due = (deps.now)() + POLL_SECS;
            loop {
                match link.wait(&*deps.now, Some(due)) {
                    None | Some(Ctl::Refresh) => break,
                    Some(Ctl::Stop) => return,
                    Some(Ctl::Event(())) => {}
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::tests::{T0, fake_clock, handle, next};
    use std::path::PathBuf;
    use std::time::Instant;

    const QUOTA: &str = include_str!("testdata/agy_usage.json");

    /// A fake `agy` that logs its arguments, prints `version` for
    /// `--version` and runs `usage` (shell) otherwise.
    fn fake(dir: &Path, version: &str, usage: &str) -> PathBuf {
        let calls = dir.join("calls");
        cli::tests::script(
            dir,
            "agy",
            &format!(
                "echo \"$*\" >> '{}'\nif [ \"$1\" = --version ]; then echo '{version}'; exit 0; fi\n{usage}",
                calls.display()
            ),
        )
    }

    fn started(tool: PathBuf, usage_timeout: Duration) -> crate::limits::tests::Probe {
        let (_, clock) = fake_clock();
        let sp = crate::limits::tests::spawner();
        let deps = Deps {
            resolve: Arc::new(move || {
                Some(Resolution {
                    path: tool.clone(),
                    env: [("PATH", "/usr/bin:/bin")].into_iter().collect(),
                })
            }),
            now: clock,
            cancel: sp.cancel.clone(),
            version_timeout: Duration::from_secs(3),
            usage_timeout,
        };
        handle(vec![spawn(deps, &sp)], &sp)
    }

    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    #[test]
    fn old_version_is_refused_before_usage_runs() {
        assert!(supports_usage("1.1.11\n"));
        assert!(supports_usage("agy 1.5.16"));
        assert!(supports_usage("v2.0.0"));
        assert!(!supports_usage("1.1.10"));
        assert!(!supports_usage("1.0.99"));
        assert!(!supports_usage("1.2"));
        assert!(!supports_usage("1.2.x"));
        assert!(!supports_usage(""));
        // Only the installed version counts, never a later token.
        assert_eq!(
            installed_version("agy 1.1.10 update available: 1.5.16"),
            Some((1, 1, 10))
        );
        assert!(!supports_usage("agy 1.1.10 update available: 1.5.16"));
        assert!(supports_usage("agy v1.5.16 (update available: 2.0.0)\n"));
        // Any other shape is refused.
        for unreadable in [
            "Antigravity CLI 1.5.16",
            "update available: 1.5.16",
            "agy version 1.5.16",
            "agy",
            "1.5.16.2",
            "1.5.16-beta",
            "1.+5.16",
            "1..16",
            "notice\n1.5.16",
        ] {
            assert_eq!(installed_version(unreadable), None, "{unreadable}");
            assert!(!supports_usage(unreadable), "{unreadable}");
        }
        for version in [
            "1.1.10",
            "agy 1.1.10 update available: 1.5.16",
            "Antigravity 1.5.16",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let tool = fake(dir.path(), version, "echo should-not-run");
            let h = started(tool, Duration::from_secs(12));
            let s = next(&h);
            assert_eq!(
                s.state,
                SourceState::Error(Failure::Outdated.reason().into()),
                "{version}"
            );
            assert_eq!(calls(dir.path()), ["--version"], "{version}");
        }
    }

    #[test]
    fn remaining_fraction_is_inverted() {
        let s = parse_usage(QUOTA.as_bytes(), T0).unwrap();
        let w: Vec<_> = s
            .windows
            .iter()
            .map(|w| {
                (
                    w.id.as_str(),
                    w.label.as_str(),
                    w.used_percent,
                    w.window_minutes,
                )
            })
            .collect();
        assert_eq!(
            w,
            [
                ("gemini-5h", "Gemini 5h", 25.0, Some(300)),
                ("gemini-weekly", "Gemini 7d", 50.0, Some(10_080)),
                ("3p-5h", "Claude/GPT 5h", 0.0, Some(300)),
                ("3p-weekly", "Claude/GPT 7d", 100.0, Some(10_080)),
            ]
        );
        assert_eq!(
            s.windows[0].resets_at,
            parse_rfc3339("2027-01-15T05:00:00Z")
        );
        assert_eq!(
            s.windows[2].resets_at,
            parse_rfc3339("2027-01-14T20:00:00Z")
        );
        assert_eq!(s.notes, [SHARED_NOTE]);
        assert_eq!(s.plan, None);
        assert_eq!(s.stale_after, 900);
        // Through the fake CLI, with the exact arguments.
        let dir = tempfile::tempdir().unwrap();
        let quota = dir.path().join("quota.json");
        std::fs::write(&quota, QUOTA).unwrap();
        let tool = fake(dir.path(), "1.5.16", &format!("cat '{}'", quota.display()));
        let h = started(tool, Duration::from_secs(12));
        let live = next(&h);
        assert_eq!(live.state, SourceState::Ok);
        assert_eq!(live.windows.len(), 4);
        assert_eq!(
            calls(dir.path()),
            [
                "--version",
                "-p /usage --output-format json --print-timeout 10s"
            ]
        );
    }

    #[test]
    fn invalid_envelope_is_unexpected() {
        let cases = [
            QUOTA.replace("SUCCESS", "ERROR"),
            QUOTA.replace("\"num_turns\":0", "\"num_turns\":1"),
            QUOTA.replace("\"name\":\"usage\"", "\"name\":\"chat\""),
            QUOTA.replace("0.75", "1.5"),
            QUOTA.replace("\"id\":\"3p-5h\"", "\"id\":\"\""),
            r#"{"status":"SUCCESS"}"#.into(),
            "I am a model answering your /usage prompt".into(),
        ];
        for case in &cases {
            assert_eq!(
                parse_usage(case.as_bytes(), T0).unwrap_err(),
                Failure::Unexpected,
                "{case}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let tool = fake(dir.path(), "1.5.16", "echo 'not json'");
        let s = next(&started(tool, Duration::from_secs(12)));
        assert_eq!(
            s.state,
            SourceState::Error(Failure::Unexpected.reason().into())
        );
    }

    #[test]
    fn timeout_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let tool = fake(dir.path(), "1.5.16", "sleep 30");
        let start = Instant::now();
        let h = started(tool, Duration::from_millis(300));
        let s = next(&h);
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(
            s.state,
            SourceState::Error(Failure::TimedOut.reason().into())
        );
        assert_eq!(s.notes, [Failure::TimedOut.reason()]);
    }
}
