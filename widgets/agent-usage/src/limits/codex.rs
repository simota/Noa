//! Codex limits from a long-lived `codex app-server` child speaking
//! newline-delimited JSON-RPC on stdio: `initialize` → notify `initialized` →
//! `account/read` → `account/rateLimits/read`; a pushed
//! `account/rateLimits/updated` triggers a re-read (after the one in flight,
//! if any). When `codex` is missing or the server fails, the newest
//! `token_count.rate_limits` sample of the latest rollouts under
//! `sessions` stands in (`Origin::Log`), or the last live value kept stale,
//! whichever is newer.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::{Value, json};

use super::cli::{self, Guarded};
use super::{Clock, Ctl, Link, POLL_SECS, RETRY_SECS, Slot, Spawner, plan_name, unavailable};
use crate::model::{Agent, Origin, SourceState, Unix, UsageSource, UsageSpend, UsageWindow};
use crate::paths::Roots;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// How much of each rollout is scanned for a `token_count`.
const LOG_TAIL_BYTES: u64 = 256 * 1024;
/// Rollouts scanned (the most recently modified); the sample with the newest
/// line timestamp among them wins.
const LOG_FILES_TRIED: usize = 3;
/// A line longer than this from the server is dropped unread.
const MAX_LINE: usize = 1 << 20;

// MARK: - Wire types (app-server camelCase; rollout logs snake_case)

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RateWindow {
    #[serde(alias = "used_percent")]
    used_percent: Option<f64>,
    #[serde(alias = "window_minutes")]
    window_duration_mins: Option<i64>,
    #[serde(alias = "resets_at")]
    resets_at: Option<f64>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Credits {
    #[serde(alias = "has_credits")]
    has_credits: Option<bool>,
    unlimited: Option<bool>,
    balance: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct IndividualLimit {
    used: Option<String>,
    limit: Option<String>,
    #[serde(alias = "remaining_percent")]
    remaining_percent: Option<f64>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    #[serde(alias = "limit_id")]
    limit_id: Option<String>,
    #[serde(alias = "limit_name")]
    limit_name: Option<String>,
    primary: Option<RateWindow>,
    secondary: Option<RateWindow>,
    credits: Option<Credits>,
    #[serde(alias = "plan_type")]
    plan_type: Option<String>,
    #[serde(alias = "individual_limit")]
    individual_limit: Option<IndividualLimit>,
    #[serde(alias = "rate_limit_reached_type")]
    rate_limit_reached_type: Option<String>,
}

#[derive(Deserialize)]
struct ResetCredit {
    status: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetCredits {
    available_count: Option<i64>,
    credits: Option<Vec<ResetCredit>>,
}

impl ResetCredits {
    /// `availableCount`, else the credits listed as available.
    fn count(&self) -> i64 {
        self.available_count.unwrap_or_else(|| {
            self.credits
                .iter()
                .flatten()
                .filter(|c| c.status.as_deref() == Some("available"))
                .count() as i64
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsResult {
    rate_limits: Snapshot,
    rate_limits_by_limit_id: Option<BTreeMap<String, Snapshot>>,
    rate_limit_reset_credits: Option<ResetCredits>,
    /// `false` means ordinary usage is stopped, whatever the percentages say.
    ordinary_usage_allowed: Option<bool>,
}

/// What the read says beyond the snapshot (a log sample has neither).
#[derive(Clone, Copy, Default)]
struct Status {
    reset_count: Option<i64>,
    usage_allowed: Option<bool>,
}

impl RateLimitsResult {
    fn status(&self) -> Status {
        Status {
            reset_count: self
                .rate_limit_reset_credits
                .as_ref()
                .map(ResetCredits::count),
            usage_allowed: self.ordinary_usage_allowed,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    email: Option<String>,
    plan_type: Option<String>,
}

#[derive(Deserialize)]
struct AccountResult {
    account: Option<Account>,
}

#[derive(Deserialize)]
struct RpcError {
    code: Option<i64>,
}

#[derive(Deserialize)]
struct Envelope {
    id: Option<i64>,
    method: Option<String>,
    error: Option<RpcError>,
    result: Option<Value>,
}

// MARK: - Normalisation

fn minutes_label(minutes: Option<i64>) -> String {
    match minutes {
        Some(m) if m > 0 && m % 1440 == 0 => format!("{}d", m / 1440),
        Some(m) if m > 0 && m % 60 == 0 => format!("{}h", m / 60),
        Some(m) if m > 0 => format!("{m}m"),
        _ => "—".into(),
    }
}

fn window(w: Option<&RateWindow>, id: String, prefix: Option<&str>) -> Option<UsageWindow> {
    let w = w?;
    let used = w.used_percent.filter(|u| u.is_finite())?;
    let duration = minutes_label(w.window_duration_mins);
    Some(UsageWindow {
        id,
        label: match prefix {
            Some(p) => format!("{p} {duration}"),
            None => duration,
        },
        used_percent: used,
        resets_at: w.resets_at.filter(|r| r.is_finite()).map(|r| r as Unix),
        window_minutes: w.window_duration_mins.filter(|m| *m > 0),
        is_active: false,
    })
}

/// "GPT-6.2-Codex-Spark" → "Spark".
fn short_label(name: Option<&str>) -> Option<String> {
    let name = name.filter(|n| !n.is_empty())?;
    Some(name.rsplit('-').next().unwrap_or(name).to_string())
}

/// "58716.3917" → "58,716.39".
pub fn amount(value: &str) -> String {
    let Ok(n) = value.trim().parse::<f64>() else {
        return value.to_string();
    };
    let fixed = format!("{:.2}", n.abs());
    let (whole, frac) = fixed.split_once('.').unwrap_or((&fixed, ""));
    let frac = frac.trim_end_matches('0');
    let mut grouped = String::new();
    for (i, d) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(d);
    }
    let sign = if n < 0.0 && !fixed.trim_matches(['0', '.']).is_empty() {
        "-"
    } else {
        ""
    };
    if frac.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{frac}")
    }
}

fn notes(snap: &Snapshot, status: Status) -> Vec<String> {
    let mut notes = Vec::new();
    match snap.rate_limit_reached_type.as_deref() {
        None if status.usage_allowed == Some(false) => notes.push("Usage blocked".into()),
        None => {}
        Some("rate_limit_reached") => notes.push("Limit reached".into()),
        Some(k) if k.contains("credits_depleted") => {
            notes.push("Workspace credits depleted".into())
        }
        Some(k) if k.contains("usage_limit_reached") => {
            notes.push("Workspace usage limit reached".into())
        }
        Some(_) => notes.push("Usage blocked".into()),
    }
    if let Some(n) = status.reset_count.filter(|n| *n > 0) {
        notes.push(format!("{n} free reset{}", if n == 1 { "" } else { "s" }));
    }
    if let Some(c) = &snap.credits
        && c.unlimited != Some(true)
        && c.has_credits == Some(true)
        && let Some(balance) = &c.balance
        && balance.trim().parse::<f64>().is_ok_and(|b| b != 0.0)
    {
        notes.push(format!("Credit balance {}", amount(balance)));
    }
    notes
}

fn source_from(
    snap: &Snapshot,
    by_id: Option<&BTreeMap<String, Snapshot>>,
    status: Status,
    now: Unix,
    origin: Origin,
) -> UsageSource {
    let mut windows: Vec<UsageWindow> = [
        window(snap.primary.as_ref(), "codex-primary".into(), None),
        window(snap.secondary.as_ref(), "codex-secondary".into(), None),
    ]
    .into_iter()
    .flatten()
    .collect();
    // Each further metered limit (Spark …) is a window pair of its own.
    for (key, other) in by_id.into_iter().flatten() {
        if Some(key) == snap.limit_id.as_ref() {
            continue;
        }
        let label = short_label(other.limit_name.as_deref()).unwrap_or_else(|| key.clone());
        windows.extend(window(
            other.primary.as_ref(),
            format!("codex-{key}-primary"),
            Some(&label),
        ));
        windows.extend(window(
            other.secondary.as_ref(),
            format!("codex-{key}-secondary"),
            Some(&label),
        ));
    }
    let spend = snap.individual_limit.as_ref().and_then(|l| {
        let remaining = l.remaining_percent.filter(|r| r.is_finite())?;
        Some(UsageSpend {
            label: "Spend".into(),
            used_percent: 100.0 - remaining,
            detail: [l.used.as_deref(), l.limit.as_deref()]
                .into_iter()
                .flatten()
                .map(amount)
                .collect::<Vec<_>>()
                .join(" / "),
        })
    });
    UsageSource {
        plan: snap.plan_type.as_deref().and_then(plan_name),
        windows,
        spend,
        notes: notes(snap, status),
        last_updated: Some(now),
        state: SourceState::Ok,
        origin,
        ..UsageSource::placeholder(Agent::Codex)
    }
}

// MARK: - Log fallback

/// The last `max` bytes of a file, starting at a line boundary.
pub(crate) fn read_tail(path: &Path, max: u64) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes).ok()?;
    if start > 0 {
        let cut = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1);
        bytes.drain(..cut);
    }
    Some(bytes)
}

fn rollouts(roots: &[PathBuf]) -> Vec<(SystemTime, PathBuf)> {
    let mut found = Vec::new();
    let mut stack: Vec<(PathBuf, u8)> = roots.iter().map(|r| (r.clone(), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if meta.is_dir() && depth < 4 {
                stack.push((path, depth + 1));
            } else if meta.is_file()
                && name.starts_with("rollout-")
                && name.ends_with(".jsonl")
                && let Ok(mtime) = meta.modified()
            {
                found.push((mtime, path));
            }
        }
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found
}

#[derive(Deserialize)]
struct LogLine {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<LogPayload>,
}

#[derive(Deserialize)]
struct LogPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    rate_limits: Option<Snapshot>,
}

/// The `token_count` with rate limits and the newest line timestamp in the
/// most recently modified rollouts, as a source sampled at that timestamp.
pub fn log_fallback(roots: &[PathBuf]) -> Option<UsageSource> {
    rollouts(roots)
        .into_iter()
        .take(LOG_FILES_TRIED)
        // Oldest first, so a tie goes to the most recently modified file.
        .rev()
        .filter_map(|(_, path)| newest_sample(&path))
        .max_by_key(|s| s.last_updated)
}

fn newest_sample(path: &Path) -> Option<UsageSource> {
    let tail = read_tail(path, LOG_TAIL_BYTES)?;
    tail.split(|b| *b == b'\n')
        .filter_map(|line| {
            if !line.windows(11).any(|w| w == b"token_count") {
                return None;
            }
            let l: LogLine = serde_json::from_slice(line).ok()?;
            let p = l.payload?;
            if l.kind.as_deref() != Some("event_msg") || p.kind.as_deref() != Some("token_count") {
                return None;
            }
            let ts = crate::time::parse_rfc3339(l.timestamp.as_deref()?)?;
            let s = source_from(&p.rate_limits?, None, Status::default(), ts, Origin::Log);
            (!s.windows.is_empty()).then_some(s)
        })
        .max_by_key(|s| s.last_updated)
}

// MARK: - Deps

pub type MakeProcess = Arc<dyn Fn() -> Option<Command> + Send + Sync>;

pub struct Deps {
    /// The `codex app-server` command; `None` when codex is not found.
    pub make_process: MakeProcess,
    pub sessions: Vec<PathBuf>,
    pub now: Clock,
    pub request_timeout: Duration,
}

impl Deps {
    pub fn system(roots: &Roots, now: Clock, resolver: Arc<cli::Resolver>) -> Deps {
        Deps {
            make_process: Arc::new(move || {
                let r = resolver.resolve("codex")?;
                let mut c = Command::new(&r.path);
                c.arg("app-server").env_clear().envs(r.env.iter());
                Some(c)
            }),
            // Limits come from live sessions only: an archived rollout is an
            // older session, whatever its mtime.
            sessions: roots
                .codex_sessions
                .iter()
                .filter(|p| p.file_name() != Some(OsStr::new("archived_sessions")))
                .cloned()
                .collect(),
            now,
            request_timeout: REQUEST_TIMEOUT,
        }
    }
}

// MARK: - Worker

pub(crate) enum Event {
    Line(u64, Vec<u8>),
    Eof(u64),
}

#[derive(PartialEq)]
enum State {
    Failed,
    Starting,
    Ready,
}

struct Worker {
    deps: Deps,
    link: Link<Event>,
    state: State,
    child: Option<Guarded>,
    stdin: Option<ChildStdin>,
    generation: u64,
    next_id: i64,
    pending: HashMap<i64, (&'static str, Instant)>,
    /// A push came while a rate-limits read was out: its answer may predate
    /// the push, so read again once it is in.
    reread: bool,
    /// Kept in memory only to notice an account switch; never published.
    account: Option<String>,
    account_plan: Option<String>,
    last_good: Option<UsageSource>,
    poll_due: Unix,
    retry_due: Option<Unix>,
    attempt: usize,
}

pub(crate) fn spawn(deps: Deps, spawner: &Spawner) -> Slot {
    spawner.spawn(Agent::Codex, move |link: Link<Event>| {
        let poll_due = (deps.now)() + POLL_SECS;
        let mut w = Worker {
            deps,
            link,
            state: State::Failed,
            child: None,
            stdin: None,
            generation: 0,
            next_id: 1,
            pending: HashMap::new(),
            reread: false,
            account: None,
            account_plan: None,
            last_good: None,
            poll_due,
            retry_due: None,
            attempt: 0,
        };
        w.run();
        w.disconnect();
    })
}

impl Worker {
    fn now(&self) -> Unix {
        (self.deps.now)()
    }

    fn run(&mut self) {
        self.connect();
        loop {
            let now = self.now();
            if now >= self.poll_due {
                self.poll_due = now + POLL_SECS;
                self.retry_due = None;
                self.refresh();
            } else if self.retry_due.is_some_and(|r| now >= r) {
                self.retry_due = None;
                self.refresh();
            }
            let late = Instant::now();
            if let Some(method) = self
                .pending
                .values()
                .find(|(_, deadline)| *deadline <= late)
                .map(|(m, _)| *m)
            {
                self.fail(format!("Codex {method} timed out"));
            }
            match self.link.wait_tick() {
                None => {}
                Some(Ctl::Stop) => return,
                Some(Ctl::Refresh) => self.refresh(),
                Some(Ctl::Event(Event::Line(g, line))) if g == self.generation => {
                    self.dispatch(&line)
                }
                Some(Ctl::Event(Event::Eof(g))) if g == self.generation => self.server_exited(),
                Some(Ctl::Event(_)) => {}
            }
        }
    }

    fn refresh(&mut self) {
        match self.state {
            State::Failed => self.connect(),
            State::Starting => {}
            State::Ready => {
                if self.pending.is_empty() {
                    self.send("account/read", json!({}));
                }
            }
        }
    }

    fn connect(&mut self) {
        self.disconnect();
        let Some(mut command) = (self.deps.make_process)() else {
            self.state = State::Failed;
            self.publish_fallback("codex not found in PATH", true);
            return;
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut guard = match Guarded::spawn(command) {
            Ok(g) => g,
            Err(_) => return self.fail("Could not start codex app-server".into()),
        };
        let stdout = guard.child().stdout.take().expect("stdout is piped");
        self.stdin = guard.child().stdin.take();
        self.child = Some(guard);
        self.state = State::Starting;
        let generation = self.generation;
        let tx = self.link.tx.clone();
        let spawned = std::thread::Builder::new()
            .name("agent-usage-codex-read".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut line = Vec::new();
                    match (&mut reader)
                        .take(MAX_LINE as u64 + 1)
                        .read_until(b'\n', &mut line)
                    {
                        Ok(0) | Err(_) => {
                            let _ = tx.send(Ctl::Event(Event::Eof(generation)));
                            return;
                        }
                        Ok(_) => {
                            if line.len() > MAX_LINE {
                                // Skip the rest of an oversized line.
                                let mut rest = Vec::new();
                                if reader.read_until(b'\n', &mut rest).is_err() {
                                    let _ = tx.send(Ctl::Event(Event::Eof(generation)));
                                    return;
                                }
                                continue;
                            }
                            if line.last() == Some(&b'\n') {
                                line.pop();
                            }
                            if tx.send(Ctl::Event(Event::Line(generation, line))).is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        if spawned.is_err() {
            return self.fail("Could not start codex app-server".into());
        }
        self.send(
            "initialize",
            json!({"clientInfo": {"name": "noa-agent-usage", "title": "Noa Agent Usage", "version": env!("CARGO_PKG_VERSION")}}),
        );
    }

    fn disconnect(&mut self) {
        self.generation += 1;
        self.pending.clear();
        self.reread = false;
        self.stdin = None;
        // Dropping the guard kills the server's group and reaps it.
        self.child = None;
    }

    fn fail(&mut self, reason: String) {
        self.disconnect();
        self.state = State::Failed;
        let now = self.now();
        let delay = RETRY_SECS[self.attempt.min(RETRY_SECS.len() - 1)];
        self.attempt += 1;
        self.retry_due = Some(now + delay);
        self.publish_fallback(&reason, false);
    }

    fn server_exited(&mut self) {
        let mut code = None;
        if let Some(mut guard) = self.child.take() {
            let until = Instant::now() + Duration::from_millis(500);
            while !guard.has_exited() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            if guard.has_exited() {
                use std::os::unix::process::ExitStatusExt;
                let s = guard.reap();
                code = s.code().or(s.signal());
            }
        }
        let code = code.map_or("unknown".into(), |c| c.to_string());
        self.fail(format!("codex app-server exited (status {code})"));
    }

    /// The newer of the last live value (stale) and the log's, with `reason`.
    fn publish_fallback(&mut self, reason: &str, unconfigured: bool) {
        let log = log_fallback(&self.deps.sessions);
        let live_at = self.last_good.as_ref().and_then(|s| s.last_updated);
        let source = match log {
            Some(mut log) if log.last_updated > live_at => {
                log.notes.insert(0, reason.to_string());
                log
            }
            _ => unavailable(Agent::Codex, self.last_good.as_ref(), reason, unconfigured),
        };
        self.publish(source);
    }

    fn publish(&mut self, source: UsageSource) {
        if !self.link.absorb_refreshes() {
            self.link.publish(source);
        }
    }

    fn send(&mut self, method: &'static str, params: Value) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending
            .insert(id, (method, Instant::now() + self.deps.request_timeout));
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    }

    fn write(&mut self, message: Value) {
        let mut line = serde_json::to_vec(&message).expect("JSON values serialise");
        line.push(b'\n');
        let ok = self
            .stdin
            .as_mut()
            .is_some_and(|s| s.write_all(&line).and_then(|()| s.flush()).is_ok());
        if !ok {
            self.fail("codex app-server closed its input".into());
        }
    }

    fn dispatch(&mut self, line: &[u8]) {
        let Ok(env) = serde_json::from_slice::<Envelope>(line) else {
            return;
        };
        let Some(id) = env.id else {
            if self.state == State::Ready
                && env.method.as_deref() == Some("account/rateLimits/updated")
            {
                if self.pending.is_empty() {
                    self.refresh();
                } else {
                    self.reread = true;
                }
            }
            return;
        };
        let Some((method, _)) = self.pending.remove(&id) else {
            return;
        };
        if let Some(e) = env.error {
            let code = e.code.map_or(String::new(), |c| format!(" ({c})"));
            return self.fail(format!("Codex {method} failed{code}"));
        }
        let Some(result) = env.result else {
            return self.fail(format!("Codex {method}: invalid response"));
        };
        match method {
            "initialize" => {
                if !result.is_object() {
                    return self.fail("Codex initialize: invalid response".into());
                }
                self.write(json!({"jsonrpc": "2.0", "method": "initialized"}));
                if self.state == State::Starting {
                    self.state = State::Ready;
                    self.refresh();
                }
            }
            "account/read" => {
                let Ok(r) = serde_json::from_value::<AccountResult>(result) else {
                    return self.fail("Codex account/read: invalid response".into());
                };
                let email = r.account.as_ref().and_then(|a| a.email.clone());
                if email != self.account {
                    // Another login: the earlier account's value is not this one's.
                    self.last_good = None;
                    self.account = email;
                }
                self.account_plan = r
                    .account
                    .and_then(|a| a.plan_type)
                    .as_deref()
                    .and_then(plan_name);
                // Sent after any push so far, so it answers them.
                self.reread = false;
                self.send("account/rateLimits/read", Value::Null);
            }
            "account/rateLimits/read" => {
                let Ok(r) = serde_json::from_value::<RateLimitsResult>(result) else {
                    return self.fail("Codex account/rateLimits/read: invalid response".into());
                };
                let now = self.now();
                let mut s = source_from(
                    &r.rate_limits,
                    r.rate_limits_by_limit_id.as_ref(),
                    r.status(),
                    now,
                    Origin::Live,
                );
                if s.plan.is_none() {
                    s.plan = self.account_plan.clone();
                }
                self.attempt = 0;
                self.retry_due = None;
                self.poll_due = now + POLL_SECS;
                self.last_good = Some(s.clone());
                self.publish(s);
                if std::mem::take(&mut self.reread) && self.pending.is_empty() {
                    self.send("account/read", json!({}));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::tests::{T0, fake_clock, handle, next, quiet, spawner};
    use std::sync::atomic::Ordering;

    const RATE_LIMITS: &str = include_str!("testdata/codex_rate_limits.json");

    /// A fake `codex app-server`: answers by method, logs what it got.
    /// `on_limits` runs after each rateLimits answer (`$n` counts them).
    fn server(dir: &Path, limits: &[&str], on_limits: &str) -> PathBuf {
        server_with(dir, limits, "", on_limits)
    }

    /// `server`, with `before_limits` run just before each rateLimits answer.
    fn server_with(dir: &Path, limits: &[&str], before_limits: &str, on_limits: &str) -> PathBuf {
        let log = dir.join("methods");
        let mut cases = String::new();
        for (i, body) in limits.iter().enumerate() {
            cases.push_str(&format!(
                "{}) body='{}' ;;\n",
                i + 1,
                body.replace('\n', "")
            ));
        }
        let body = format!(
            r#"n=0
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -nE 's/.*"id":([0-9]+).*/\1/p')
  method=$(printf '%s' "$line" | sed -nE 's/.*"method":"([^"]*)".*/\1/p')
  echo "$method" >> '{log}'
  case "$method" in
    initialized) ready=yes ;;
    initialize) printf '{{"id":%s,"result":{{"userAgent":"synthetic"}}}}\n' "$id" ;;
    account/read)
      if [ "$ready" != yes ]; then
        printf '{{"id":%s,"error":{{"code":-32002,"message":"Not initialized"}}}}\n' "$id"
        continue
      fi
      printf '{{"id":%s,"result":{{"account":{{"type":"chatgpt","email":"synthetic@example.test","planType":"plus"}}}}}}\n' "$id" ;;
    account/rateLimits/read)
      n=$((n+1))
      case $n in
{cases}      *) body='{last}' ;;
      esac
      {before_limits}
      printf '{{"id":%s,"result":%s}}\n' "$id" "$body"
      {on_limits}
      ;;
  esac
done
"#,
            log = log.display(),
            last = limits.last().unwrap().replace('\n', ""),
        );
        cli::tests::script(dir, "codex", &body)
    }

    fn deps_for(script: Option<PathBuf>, sessions: &Path, clock: Clock) -> Deps {
        Deps {
            make_process: Arc::new(move || {
                let mut c = Command::new(script.as_ref()?);
                c.arg("app-server").env_clear().env("PATH", "/usr/bin:/bin");
                Some(c)
            }),
            sessions: vec![sessions.to_path_buf()],
            now: clock,
            request_timeout: Duration::from_secs(5),
        }
    }

    fn simple(used: f64) -> String {
        format!(
            r#"{{"rateLimits":{{"limitId":"codex","primary":{{"usedPercent":{used},"windowDurationMins":300,"resetsAt":1800009000}},"secondary":null,"planType":"pro"}}}}"#
        )
    }

    fn methods(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("methods"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    #[test]
    fn app_server_handshake_then_rate_limits() {
        let dir = tempfile::tempdir().unwrap();
        let script = server(dir.path(), &[RATE_LIMITS], "");
        let (_, clock) = fake_clock();
        let sp = spawner();
        let h = handle(
            vec![spawn(
                deps_for(Some(script), &dir.path().join("none"), clock),
                &sp,
            )],
            &sp,
        );
        let s = next(&h);
        assert_eq!(s.state, SourceState::Ok);
        assert_eq!(s.origin, Origin::Live);
        assert_eq!(s.last_updated, Some(T0));
        assert_eq!(s.plan.as_deref(), Some("Pro"));
        assert_eq!(s.stale_after, 600);
        let w = &s.windows[0];
        assert_eq!((w.id.as_str(), w.label.as_str()), ("codex-primary", "5h"));
        assert_eq!(w.used_percent, 56.0);
        assert_eq!(w.resets_at, Some(1_800_009_000));
        assert_eq!(w.window_minutes, Some(300));
        assert_eq!(
            methods(dir.path()),
            [
                "initialize",
                "initialized",
                "account/read",
                "account/rateLimits/read"
            ]
        );
        // The account e-mail is nowhere in what is published.
        assert!(!format!("{s:?}").contains("example.test"));
        drop(h);
    }

    #[test]
    fn push_update_triggers_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let first = simple(50.0);
        let second = simple(70.0);
        let push = r#"if [ $n = 1 ]; then printf '{"method":"account/rateLimits/updated","params":{}}\n'; fi"#;
        let script = server(dir.path(), &[&first, &second], push);
        let (_, clock) = fake_clock();
        let sp = spawner();
        let h = handle(
            vec![spawn(
                deps_for(Some(script), &dir.path().join("none"), clock),
                &sp,
            )],
            &sp,
        );
        let mut got = vec![next(&h)];
        if got[0].windows[0].used_percent == 50.0 {
            got.push(next(&h));
        }
        assert_eq!(got.last().unwrap().windows[0].used_percent, 70.0);
        assert_eq!(
            methods(dir.path())
                .iter()
                .filter(|m| *m == "account/rateLimits/read")
                .count(),
            2
        );
        quiet(&h);
    }

    #[test]
    fn extra_limit_ids_become_labelled_windows() {
        let r: RateLimitsResult = serde_json::from_str(RATE_LIMITS).unwrap();
        let s = source_from(
            &r.rate_limits,
            r.rate_limits_by_limit_id.as_ref(),
            r.status(),
            T0,
            Origin::Live,
        );
        let w: Vec<_> = s
            .windows
            .iter()
            .map(|w| (w.id.as_str(), w.label.as_str()))
            .collect();
        assert_eq!(
            w,
            [
                ("codex-primary", "5h"),
                ("codex-secondary", "7d"),
                ("codex-spark-primary", "Spark 5h"),
                ("codex-spark-secondary", "Spark 7d"),
            ]
        );
        assert_eq!(s.windows[3].used_percent, 99.0);
        assert_eq!(
            s.notes,
            ["Limit reached", "2 free resets", "Credit balance 1,234.5"]
        );
        let spend = s.spend.unwrap();
        assert_eq!(spend.label, "Spend");
        assert_eq!(spend.used_percent, 75.0);
        assert_eq!(spend.detail, "58,716.39 / 78,288.52");
        assert_eq!(amount("-0.001"), "0");
        assert_eq!(amount("x"), "x");
    }

    #[test]
    fn server_exit_keeps_last_value_stale() {
        let dir = tempfile::tempdir().unwrap();
        let script = server(dir.path(), &[&simple(42.0)], "exit 0");
        let (at, clock) = fake_clock();
        let sp = spawner();
        let h = handle(
            vec![spawn(
                deps_for(Some(script), &dir.path().join("none"), clock),
                &sp,
            )],
            &sp,
        );
        let good = next(&h);
        assert_eq!(good.state, SourceState::Ok);
        at.store(T0 + 5, Ordering::SeqCst);
        let stale = next(&h);
        assert_eq!(stale.state, SourceState::Stale { since: T0 });
        assert_eq!(stale.notes, ["codex app-server exited (status 0)"]);
        assert_eq!(stale.windows, good.windows);
        // It reconnects after the back-off (30 s) and is fresh again.
        at.store(T0 + 35, Ordering::SeqCst);
        let again = next(&h);
        assert_eq!(again.state, SourceState::Ok);
        assert_eq!(again.last_updated, Some(T0 + 35));
        assert_eq!(
            methods(dir.path())
                .iter()
                .filter(|m| *m == "initialize")
                .count(),
            2
        );
    }

    fn rollout(dir: &Path, name: &str, lines: &[String], mtime: SystemTime) {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    fn token_count(ts: &str, used: Option<f64>) -> String {
        let limits = match used {
            Some(u) => format!(
                r#"{{"limit_id":"codex","limit_name":null,"primary":{{"used_percent":{u},"window_minutes":300,"resets_at":1800009000}},"secondary":{{"used_percent":20.0,"window_minutes":10080,"resets_at":1800400000}},"credits":{{"has_credits":false,"unlimited":false,"balance":"0"}},"individual_limit":null,"plan_type":"pro","rate_limit_reached_type":null}}"#
            ),
            None => "null".into(),
        };
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":1}}}},"rate_limits":{limits}}}}}"#
        )
    }

    #[test]
    fn log_fallback_reads_the_newest_token_count() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let day = sessions.join("2027/01/15");
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(T0 as u64);
        rollout(
            &sessions.join("2027/01/14"),
            "rollout-2027-01-14T10-00-00-old.jsonl",
            &[token_count("2027-01-14T10:00:00Z", Some(90.0))],
            base - Duration::from_secs(86_400),
        );
        rollout(
            &day,
            "rollout-2027-01-15T09-00-00-new.jsonl",
            &[
                r#"{"timestamp":"2027-01-15T09:00:00Z","type":"session_meta","payload":{"id":"x"}}"#.into(),
                token_count("2027-01-15T09:05:00.250Z", Some(12.5)),
                token_count("2027-01-15T09:06:00Z", Some(13.0)),
                token_count("2027-01-15T09:07:00Z", None),
                r#"{"timestamp":"2027-01-15T09:08:00Z","type":"response_item","payload":{"type":"message","role":"assistant"}}"#.into(),
                "{not json".into(),
            ],
            base,
        );
        std::fs::write(day.join("notes.txt"), "token_count").unwrap();
        let s = log_fallback(std::slice::from_ref(&sessions)).unwrap();
        assert_eq!(s.origin, Origin::Log);
        assert_eq!(
            s.last_updated,
            crate::time::parse_rfc3339("2027-01-15T09:06:00Z")
        );
        let w: Vec<_> = s
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.used_percent))
            .collect();
        assert_eq!(w, [("5h", 13.0), ("7d", 20.0)]);
        assert_eq!(s.windows[1].resets_at, Some(1_800_400_000));
        assert_eq!(s.plan.as_deref(), Some("Pro"));
        assert!(s.notes.is_empty());
        // Codex missing: the card shows the log's windows with the reason.
        let (_, clock) = fake_clock();
        let sp = spawner();
        let h = handle(vec![spawn(deps_for(None, &sessions, clock), &sp)], &sp);
        let card = next(&h);
        assert_eq!(card.origin, Origin::Log);
        assert_eq!(card.notes, ["codex not found in PATH"]);
        assert_eq!(card.windows.len(), 2);
        // Stale styling once past stale_after (it is a 2027 sample read in 2027).
        let later = card.last_updated.unwrap() + 601;
        assert!(matches!(card.aged_state(later), SourceState::Stale { .. }));
        // Nothing at all: unconfigured.
        assert!(log_fallback(&[dir.path().join("absent")]).is_none());
    }

    #[test]
    fn push_during_a_read_reads_again_after_it() {
        // The server builds the old answer, pushes the update, then sends
        // the old answer: it must be read again.
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (simple(50.0), simple(70.0));
        let push = r#"if [ $n = 1 ]; then printf '{"method":"account/rateLimits/updated","params":{}}\n'; fi"#;
        let script = server_with(dir.path(), &[&first, &second], push, "");
        let (_, clock) = fake_clock();
        let sp = spawner();
        let h = handle(
            vec![spawn(
                deps_for(Some(script), &dir.path().join("none"), clock),
                &sp,
            )],
            &sp,
        );
        assert_eq!(next(&h).windows[0].used_percent, 50.0);
        assert_eq!(next(&h).windows[0].used_percent, 70.0);
        assert_eq!(
            methods(dir.path()),
            [
                "initialize",
                "initialized",
                "account/read",
                "account/rateLimits/read",
                "account/read",
                "account/rateLimits/read"
            ]
        );
        quiet(&h);
    }

    fn notes_of(json: &str) -> Vec<String> {
        let r: RateLimitsResult = serde_json::from_str(json).unwrap();
        source_from(&r.rate_limits, None, r.status(), T0, Origin::Live).notes
    }

    #[test]
    fn usage_stopped_and_free_resets_are_noted_without_codes_or_counts() {
        let low = r#""rateLimits":{"limitId":"codex","primary":{"usedPercent":12,"windowDurationMins":300}}"#;
        let credits = r#""credits":[{"id":"a","status":"available","expiresAt":4000000000},{"id":"b","status":"used"}]"#;
        // Stopped with no reason code, low usage; one available credit and
        // no availableCount.
        assert_eq!(
            notes_of(&format!(
                r#"{{{low},"ordinaryUsageAllowed":false,"rateLimitResetCredits":{{{credits}}}}}"#
            )),
            ["Usage blocked", "1 free reset"]
        );
        // availableCount, when given, is the count.
        assert_eq!(
            notes_of(&format!(
                r#"{{{low},"ordinaryUsageAllowed":true,"rateLimitResetCredits":{{"availableCount":3,{credits}}}}}"#
            )),
            ["3 free resets"]
        );
        // Unknown or allowed: no stop note; a reason code names the stop.
        assert!(notes_of(&format!(r#"{{{low},"ordinaryUsageAllowed":null}}"#)).is_empty());
        let reached = r#""rateLimits":{"limitId":"codex","primary":{"usedPercent":12,"windowDurationMins":300},"rateLimitReachedType":"workspace_member_credits_depleted"}"#;
        assert_eq!(
            notes_of(&format!(r#"{{{reached},"ordinaryUsageAllowed":false}}"#)),
            ["Workspace credits depleted"]
        );
    }

    #[test]
    fn log_fallback_picks_the_newest_sample_not_the_newest_file() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(T0 as u64);
        // Touched last, but its samples are older.
        rollout(
            &sessions.join("2027/01/14"),
            "rollout-2027-01-14T08-00-00-touched.jsonl",
            &[token_count("2027-01-15T08:00:00Z", Some(80.0))],
            base,
        );
        rollout(
            &sessions.join("2027/01/15"),
            "rollout-2027-01-15T09-00-00-current.jsonl",
            &[
                token_count("2027-01-15T09:30:00Z", Some(30.0)),
                token_count("2027-01-15T09:00:00Z", Some(25.0)),
            ],
            base - Duration::from_secs(60),
        );
        let s = log_fallback(std::slice::from_ref(&sessions)).unwrap();
        assert_eq!(
            s.last_updated,
            crate::time::parse_rfc3339("2027-01-15T09:30:00Z")
        );
        assert_eq!(s.windows[0].used_percent, 30.0);
        // Archived rollouts are not read for limits, however new their
        // mtime or samples.
        let codex = dir.path().join("codex");
        let archived = codex.join("archived_sessions");
        rollout(
            &archived,
            "rollout-2027-01-15T10-00-00-archived.jsonl",
            &[token_count("2027-01-15T11:00:00Z", Some(99.0))],
            base + Duration::from_secs(60),
        );
        rollout(
            &codex.join("sessions/2027/01/15"),
            "rollout-2027-01-15T09-00-00-live.jsonl",
            &[token_count("2027-01-15T09:30:00Z", Some(30.0))],
            base,
        );
        let roots = Roots {
            codex_sessions: vec![codex.join("sessions"), archived],
            ..Roots::default()
        };
        let resolver = Arc::new(cli::Resolver::new(cli::Env::default(), vec![], Vec::new));
        let (_, clock) = fake_clock();
        let deps = Deps::system(&roots, clock, resolver);
        assert_eq!(deps.sessions, [codex.join("sessions")]);
        let s = log_fallback(&deps.sessions).unwrap();
        assert_eq!(s.windows[0].used_percent, 30.0);
    }
}
