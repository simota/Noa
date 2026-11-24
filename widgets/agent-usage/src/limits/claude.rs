//! Claude.ai subscription limits from the (unofficial) OAuth usage endpoint,
//! with the token Claude Code already holds.
//!
//! The credential (macOS keychain item, else `.credentials.json`) is only ever
//! read, and re-read every cycle. The token is never refreshed: the refresh
//! token rotates server-side, so refreshing here would break the user's Claude
//! Code login. An expired token, or a 401/403, turns the card stale with
//! "Token expired (launch Claude Code to renew)". The token is held in a
//! `Secret` (redacted `Debug`, no `Display`) and reaches only curl's stdin.

use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use super::cli::{self, Cancel, Resolver};
use super::curl::{self, HttpFn, HttpResponse};
use super::{Clock, Ctl, Link, Mono, POLL_SECS, RETRY_SECS, Slot, Spawner, in_secs, unavailable};
use crate::model::{
    Agent, Origin, SourceState, Unix, UsageBucket, UsageSource, UsageSpend, UsageWindow,
};
use crate::paths::Roots;
use crate::time::parse_rfc3339;

/// Below this gap the endpoint drops the caller into a throttled bucket; it
/// holds for manual refreshes too.
pub const MIN_GAP_SECS: i64 = 180;
const EXPIRED: &str = "Token expired (launch Claude Code to renew)";
const MAX_CREDENTIAL_BYTES: u64 = 1 << 20;
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(10);
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// A token: never printed. `Debug` shows only that it exists.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credential {
    Token {
        token: Secret,
        plan: Option<String>,
    },
    Expired,
    /// No credential exists (not signed in).
    Missing,
    /// It exists but could not be read; the reason names no content.
    Unreadable(String),
    /// Read, but not in Claude Code's shape.
    Malformed,
}

#[derive(Deserialize)]
struct CredentialFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OAuth>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuth {
    access_token: Option<String>,
    expires_at: Option<f64>,
    subscription_type: Option<String>,
}

/// Claude Code's credential JSON (file or keychain item). `now` in seconds.
pub fn parse_credential(text: &[u8], now: Unix) -> Credential {
    let Ok(file) = serde_json::from_slice::<CredentialFile>(text) else {
        return Credential::Malformed;
    };
    let Some(oauth) = file.oauth else {
        return Credential::Malformed;
    };
    // A token must be one plain header value.
    let Some(token) = oauth
        .access_token
        .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_graphic()))
    else {
        return Credential::Malformed;
    };
    if oauth
        .expires_at
        .is_some_and(|ms| ms <= (now as f64) * 1000.0)
    {
        return Credential::Expired;
    }
    Credential::Token {
        token: Secret(token),
        plan: oauth
            .subscription_type
            .as_deref()
            .and_then(super::plan_name),
    }
}

/// Reads the credentials file read-only (never created, written or chmod-ed).
pub fn read_credential_file(path: &Path, now: Unix) -> Credential {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Credential::Missing,
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            return Credential::Unreadable(
                "Credentials file unreadable (permission denied)".into(),
            );
        }
        Err(_) => return Credential::Unreadable("Credentials file unreadable".into()),
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Credential::Unreadable("Credentials file unreadable".into());
    }
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Credential::Unreadable("Credentials file too large".into());
    }
    parse_credential(&bytes, now)
}

/// macOS: `security find-generic-password -s "Claude Code-credentials" -w`.
/// `None` when the item is absent or `security` fails (the file is next).
fn read_keychain(now: Unix, cancel: &Cancel) -> Option<Credential> {
    let out = cli::run(
        Path::new("/usr/bin/security"),
        &[
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ],
        None,
        None,
        KEYCHAIN_TIMEOUT,
        &[0],
        Some(cancel),
    )
    .ok()?;
    Some(parse_credential(out.trim_ascii(), now))
}

/// `version` of the newest record in the newest `projects/**/*.jsonl`.
pub fn version_from_logs(projects: &Path) -> Option<String> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut stack = vec![(projects.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() && depth < 3 {
                stack.push((path, depth + 1));
            } else if meta.is_file()
                && path.extension().is_some_and(|e| e == "jsonl")
                && let Ok(mtime) = meta.modified()
                && newest.as_ref().is_none_or(|(t, _)| mtime > *t)
            {
                newest = Some((mtime, path));
            }
        }
    }
    let tail = super::codex::read_tail(&newest?.1, 256 * 1024)?;
    #[derive(Deserialize)]
    struct Record {
        version: Option<String>,
    }
    tail.split(|b| *b == b'\n').rev().find_map(|line| {
        serde_json::from_slice::<Record>(line)
            .ok()?
            .version
            .filter(|v| valid_version(v))
    })
}

fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 32
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

fn version_from_cli(resolver: &Resolver, cancel: &Cancel) -> Option<String> {
    let r = resolver.resolve("claude")?;
    let out = cli::run(
        &r.path,
        &["--version"],
        Some(&r.env),
        None,
        VERSION_TIMEOUT,
        &[0],
        Some(cancel),
    )
    .ok()?;
    String::from_utf8_lossy(&out)
        .split_whitespace()
        .next()
        .filter(|v| valid_version(v))
        .map(String::from)
}

pub type CredentialFn = Arc<dyn Fn() -> Credential + Send + Sync>;
/// Finds curl and returns the request function, or `None` when curl is not
/// found. Called on the source's thread (the lookup may take seconds).
pub type MakeHttp = Arc<dyn Fn() -> Option<HttpFn> + Send + Sync>;

pub struct Deps {
    pub http: MakeHttp,
    pub credential: CredentialFn,
    pub version: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    /// Sample times and `Retry-After` dates.
    pub now: Clock,
    /// Every schedule: the request gap, back-off and polls.
    pub mono: Mono,
}

impl Deps {
    pub fn system(
        roots: &Roots,
        now: Clock,
        mono: Mono,
        cancel: Cancel,
        resolver: Arc<Resolver>,
    ) -> Deps {
        let path = roots.claude_credentials.clone();
        let (clock, keychain_cancel) = (now.clone(), cancel.clone());
        let projects = roots.claude_projects.clone();
        let (version_cancel, version_resolver) = (cancel.clone(), resolver.clone());
        Deps {
            http: Arc::new(move || curl::system_http(&resolver, cancel.clone())),
            credential: Arc::new(move || {
                let now = clock();
                if cfg!(target_os = "macos")
                    && let Some(c) = read_keychain(now, &keychain_cancel)
                {
                    return c;
                }
                match &path {
                    Some(p) => read_credential_file(p, now),
                    None => Credential::Missing,
                }
            }),
            version: Arc::new(move || {
                projects
                    .as_deref()
                    .and_then(version_from_logs)
                    .or_else(|| version_from_cli(&version_resolver, &version_cancel))
            }),
            now,
            mono,
        }
    }
}

// MARK: - Response

#[derive(Deserialize, Default)]
struct Usage {
    five_hour: Option<Flat>,
    seven_day: Option<Flat>,
    limits: Option<Vec<Limit>>,
    spend: Option<Spend>,
}

#[derive(Deserialize)]
struct Flat {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct Limit {
    kind: Option<String>,
    percent: Option<f64>,
    resets_at: Option<String>,
    is_active: Option<bool>,
    scope: Option<Scope>,
}

#[derive(Deserialize)]
struct Scope {
    model: Option<ScopeModel>,
}

#[derive(Deserialize)]
struct ScopeModel {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct Money {
    amount_minor: Option<f64>,
    currency: Option<String>,
    exponent: Option<i32>,
}

#[derive(Deserialize)]
struct Spend {
    enabled: Option<bool>,
    used: Option<Money>,
    limit: Option<Money>,
    percent: Option<f64>,
}

fn window(
    id: &str,
    label: &str,
    used: f64,
    resets_at: Option<&str>,
    minutes: i64,
    active: bool,
) -> UsageWindow {
    UsageWindow {
        id: id.into(),
        label: label.into(),
        used_percent: used,
        resets_at: resets_at.and_then(parse_rfc3339),
        window_minutes: Some(minutes),
        is_active: active,
    }
}

/// The response body as a source, or the reason it is not one.
fn normalise(body: &[u8], now: Unix) -> Result<UsageSource, &'static str> {
    let usage: Usage = serde_json::from_slice(body).map_err(|_| "Could not read the response")?;
    let mut windows = Vec::new();
    let mut buckets = Vec::new();
    for limit in usage.limits.unwrap_or_default() {
        let Some(percent) = limit.percent.filter(|p| p.is_finite()) else {
            continue;
        };
        let active = limit.is_active == Some(true);
        let reset = limit.resets_at.as_deref();
        match limit.kind.as_deref() {
            Some("session") => windows.push(window("claude-5h", "5h", percent, reset, 300, active)),
            Some("weekly_all") => {
                windows.push(window("claude-7d", "7d", percent, reset, 10_080, active))
            }
            kind => {
                // weekly_scoped and any later kind: a bucket under the 7d row.
                let name = limit
                    .scope
                    .and_then(|s| s.model)
                    .and_then(|m| m.display_name)
                    .or(kind.map(String::from))
                    .unwrap_or_else(|| "scoped".into());
                buckets.push(UsageBucket {
                    id: format!("claude-{}", name.to_lowercase().replace(' ', "-")),
                    label: name,
                    used_percent: percent,
                });
            }
        }
    }
    if windows.is_empty() {
        for (flat, id, label, minutes) in [
            (&usage.five_hour, "claude-5h", "5h", 300),
            (&usage.seven_day, "claude-7d", "7d", 10_080),
        ] {
            if let Some(f) = flat
                && let Some(u) = f.utilization.filter(|u| u.is_finite())
            {
                windows.push(window(id, label, u, f.resets_at.as_deref(), minutes, false));
            }
        }
    }
    if windows.is_empty() {
        return Err("No limit data returned");
    }
    Ok(UsageSource {
        windows,
        buckets,
        spend: spend_row(usage.spend),
        last_updated: Some(now),
        state: SourceState::Ok,
        origin: Origin::Live,
        ..UsageSource::placeholder(Agent::Claude)
    })
}

fn spend_row(spend: Option<Spend>) -> Option<UsageSpend> {
    let spend = spend.filter(|s| s.enabled == Some(true))?;
    let used = spend.used?.amount_minor?;
    let limit = spend.limit?;
    let limit_minor = limit.amount_minor.filter(|l| *l > 0.0)?;
    let divisor = 10f64.powi(limit.exponent.unwrap_or(2));
    let symbol = match limit.currency.as_deref().unwrap_or("USD") {
        "USD" => "$".to_string(),
        other => format!("{other} "),
    };
    Some(UsageSpend {
        label: "Credits".into(),
        used_percent: spend.percent.unwrap_or(used / limit_minor * 100.0),
        detail: format!("{symbol}{:.2}", used / divisor),
    })
}

// MARK: - Worker

fn secs(s: i64) -> Duration {
    Duration::from_secs(s.max(0) as u64)
}

/// Whole seconds, rounded up (a note never promises a request too early).
fn ceil_secs(d: Duration) -> i64 {
    let whole = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
    whole.saturating_add(i64::from(d.subsec_nanos() > 0))
}

struct Worker {
    deps: Deps,
    link: Link<()>,
    /// Found on first use.
    http: Option<HttpFn>,
    last_good: Option<UsageSource>,
    last_published: Option<UsageSource>,
    plan: Option<String>,
    user_agent: Option<String>,
    /// When the last request that may have reached the server started.
    last_sent: Option<Duration>,
    backoff_until: Option<Duration>,
    attempt: usize,
    /// The next fetch.
    due: Duration,
}

pub(crate) fn spawn(deps: Deps, spawner: &Spawner) -> Slot {
    spawner.spawn(Agent::Claude, move |link: Link<()>| {
        let due = (deps.mono)();
        Worker {
            deps,
            link,
            http: None,
            last_good: None,
            last_published: None,
            plan: None,
            user_agent: None,
            last_sent: None,
            backoff_until: None,
            attempt: 0,
            due,
        }
        .run()
    })
}

impl Worker {
    fn mono(&self) -> Duration {
        (self.deps.mono)()
    }

    /// The earliest a request may go out: 180 s after the start of the last
    /// one that may have reached the server, and not inside a back-off.
    fn earliest(&self) -> Duration {
        let gap = self
            .last_sent
            .map_or(Duration::ZERO, |t| t.saturating_add(secs(MIN_GAP_SECS)));
        gap.max(self.backoff_until.unwrap_or(Duration::ZERO))
    }

    fn run(mut self) {
        loop {
            match self.link.wait(&*self.deps.mono, Some(self.due)) {
                None => {
                    if self.mono() < self.earliest() {
                        self.due = self.earliest();
                    } else {
                        self.cycle();
                    }
                }
                // Also one pressed while a request was out: it is answered
                // by the next request, not by the one already sent.
                Some(Ctl::Refresh) => {
                    let now = self.mono();
                    let earliest = self.earliest();
                    if now >= earliest {
                        self.cycle();
                    } else {
                        // Deferred: the card says when the request runs.
                        self.due = self.due.min(earliest);
                        self.publish_deferred(earliest - now);
                    }
                }
                Some(Ctl::Stop) => return,
                Some(Ctl::Event(())) => {}
            }
        }
    }

    fn publish_deferred(&mut self, wait: Duration) {
        let note = format!(
            "Refresh deferred (next request {})",
            in_secs(ceil_secs(wait))
        );
        let mut s = self
            .last_published
            .clone()
            .unwrap_or_else(|| UsageSource::placeholder(Agent::Claude));
        s.notes.retain(|n| !n.starts_with("Refresh deferred"));
        s.notes.push(note);
        self.link.publish(s);
    }

    fn publish(&mut self, mut source: UsageSource) {
        if self.link.stopped() {
            return;
        }
        if source.plan.is_none() {
            source.plan = self.plan.clone();
        }
        self.last_published = Some(source.clone());
        self.link.publish(source);
    }

    fn unavailable(&mut self, reason: &str, unconfigured: bool) {
        let s = unavailable(Agent::Claude, self.last_good.as_ref(), reason, unconfigured);
        self.publish(s);
    }

    /// One fetch cycle: read the credential, request, publish; sets `due`.
    fn cycle(&mut self) {
        self.due = self.mono().saturating_add(secs(POLL_SECS));
        let token = match (self.deps.credential)() {
            Credential::Token { token, plan } => {
                self.plan = plan;
                token
            }
            Credential::Expired => return self.unavailable(EXPIRED, false),
            Credential::Missing => {
                // Signed out: an earlier account's values and plan are not
                // this one's.
                self.last_good = None;
                self.plan = None;
                return self
                    .unavailable("Not signed in to Claude Code (run claude and log in)", true);
            }
            Credential::Unreadable(reason) => return self.unavailable(&reason, false),
            Credential::Malformed => {
                return self.unavailable(
                    "Credential format not recognised (Claude Code may have changed it)",
                    false,
                );
            }
        };
        if self.http.is_none() {
            self.http = (self.deps.http)();
        }
        let Some(http) = self.http.clone() else {
            return self.unavailable("curl not found", false);
        };
        let version = self.deps.version.clone();
        let ua = self
            .user_agent
            .get_or_insert_with(|| {
                format!(
                    "claude-code/{}",
                    version().unwrap_or_else(|| "0.0.0".into())
                )
            })
            .clone();
        let started = self.mono();
        let response: HttpResponse = http(curl::USAGE_URL, token.expose(), &ua);
        drop(token);
        if self.link.stopped() {
            return;
        }
        if response.sent {
            self.last_sent = Some(started);
        }
        match response.status {
            200 => match normalise(&response.body, (self.deps.now)()) {
                Ok(source) => {
                    self.attempt = 0;
                    self.backoff_until = None;
                    let mut source = source;
                    source.plan = self.plan.clone();
                    self.last_good = Some(source.clone());
                    self.publish(source);
                }
                Err(reason) => self.unavailable(reason, false),
            },
            0 | 429 => {
                let delay = if response.status == 429 {
                    curl::retry_after_secs(response.retry_after.as_deref(), (self.deps.now)())
                } else {
                    None
                }
                .unwrap_or(RETRY_SECS[self.attempt.min(RETRY_SECS.len() - 1)]);
                self.attempt += 1;
                let now = self.mono();
                self.backoff_until = Some(now.saturating_add(secs(delay)));
                self.due = self.earliest();
                let what = match (response.status, response.sent) {
                    (0, false) => "Cannot connect",
                    (0, true) => "No response",
                    _ => "Rate limited",
                };
                let wait = self.due.saturating_sub(now);
                let reason = format!("{what} (retry {})", in_secs(ceil_secs(wait)));
                self.unavailable(&reason, false);
            }
            401 | 403 => self.unavailable(EXPIRED, false),
            status => self.unavailable(&format!("Unavailable (HTTP {status})"), false),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::limits::tests::{T0, fake_clock, handle, next, quiet, spawner, until};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    pub(crate) const USAGE: &str = include_str!("testdata/claude_usage.json");
    const FLAT: &str = include_str!("testdata/claude_usage_flat.json");
    const TOKEN: &str = "sk-ant-oat01-SYNTHETIC";

    pub(crate) fn ok(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            sent: true,
            retry_after: None,
            body: body.as_bytes().to_vec(),
        }
    }

    fn status(code: u16, retry: Option<&str>) -> HttpResponse {
        HttpResponse {
            status: code,
            sent: true,
            retry_after: retry.map(String::from),
            body: b"{\"error\":\"synthetic\"}".to_vec(),
        }
    }

    pub(crate) struct Fake {
        pub requests: Arc<Mutex<Vec<(String, String)>>>,
        pub credential: Arc<Mutex<Credential>>,
    }

    fn token(plan: Option<&str>) -> Credential {
        Credential::Token {
            token: Secret(TOKEN.into()),
            plan: plan.map(String::from),
        }
    }

    /// Monotonic time that follows the fake wall clock from `T0`.
    fn mono_of(clock: &Clock) -> Mono {
        let clock = clock.clone();
        Arc::new(move || secs(clock() - T0))
    }

    pub(crate) fn deps(clock: Clock, responses: Vec<HttpResponse>) -> (Deps, Fake) {
        let fake = Fake {
            requests: Arc::default(),
            credential: Arc::new(Mutex::new(token(Some("Max")))),
        };
        let (requests, credential) = (fake.requests.clone(), fake.credential.clone());
        let queue = Arc::new(Mutex::new(responses));
        let http: HttpFn = Arc::new(move |_url: &str, token: &str, ua: &str| {
            requests.lock().unwrap().push((token.into(), ua.into()));
            let mut q = queue.lock().unwrap();
            if q.is_empty() {
                HttpResponse::no_connection()
            } else {
                q.remove(0)
            }
        });
        let deps = Deps {
            http: Arc::new(move || Some(http.clone())),
            credential: Arc::new(move || credential.lock().unwrap().clone()),
            version: Arc::new(|| Some("9.9.9".into())),
            mono: mono_of(&clock),
            now: clock,
        };
        (deps, fake)
    }

    fn started(
        responses: Vec<HttpResponse>,
    ) -> (crate::limits::tests::Probe, Fake, Arc<AtomicI64>) {
        let (at, clock) = fake_clock();
        let (deps, fake) = deps(clock, responses);
        let sp = spawner();
        let h = handle(vec![spawn(deps, &sp)], &sp);
        (h, fake, at)
    }

    #[test]
    fn limits_array_normalises_to_5h_7d_buckets_and_spend() {
        let s = normalise(USAGE.as_bytes(), T0).unwrap();
        let ids: Vec<_> = s
            .windows
            .iter()
            .map(|w| (w.id.as_str(), w.label.as_str()))
            .collect();
        assert_eq!(ids, [("claude-5h", "5h"), ("claude-7d", "7d")]);
        assert_eq!(s.windows[0].used_percent, 25.0);
        assert_eq!(s.windows[0].window_minutes, Some(300));
        assert_eq!(
            s.windows[0].resets_at,
            parse_rfc3339("2027-01-15T05:00:00Z")
        );
        assert!(s.windows[0].is_active && !s.windows[1].is_active);
        assert_eq!(s.windows[1].window_minutes, Some(10_080));
        assert_eq!(
            s.windows[1].resets_at,
            parse_rfc3339("2027-01-18T00:00:00+00:00")
        );
        assert_eq!(s.buckets.len(), 1);
        assert_eq!(s.buckets[0].label, "Fable");
        assert_eq!(s.buckets[0].id, "claude-fable");
        assert_eq!(s.buckets[0].used_percent, 12.0);
        let spend = s.spend.unwrap();
        assert_eq!(spend.label, "Credits");
        assert_eq!(spend.detail, "$79.71");
        assert!((spend.used_percent - 53.14).abs() < 0.01);
        assert_eq!(s.state, SourceState::Ok);
        assert_eq!(s.last_updated, Some(T0));
        assert_eq!(s.stale_after, 900);
        assert_eq!(s.origin, Origin::Live);
        // Through the worker, with the mandatory User-Agent.
        let (h, fake, _) = started(vec![ok(USAGE)]);
        assert_eq!(next(&h).windows.len(), 2);
        assert_eq!(
            fake.requests.lock().unwrap()[0],
            (TOKEN.to_string(), "claude-code/9.9.9".to_string())
        );
    }

    #[test]
    fn flat_fallback_when_no_limits() {
        let s = normalise(FLAT.as_bytes(), T0).unwrap();
        let w: Vec<_> = s
            .windows
            .iter()
            .map(|w| (w.id.as_str(), w.used_percent))
            .collect();
        assert_eq!(w, [("claude-5h", 40.0), ("claude-7d", 61.5)]);
        assert_eq!(
            s.windows[1].resets_at,
            parse_rfc3339("2027-01-18T00:00:00.123456+09:00")
        );
        assert!(s.buckets.is_empty() && s.spend.is_none());
        assert_eq!(normalise(b"{}", T0).unwrap_err(), "No limit data returned");
        assert_eq!(
            normalise(b"not json", T0).unwrap_err(),
            "Could not read the response"
        );
    }

    #[test]
    fn expired_token_is_stale_not_error_after_a_good_sample() {
        let (h, fake, at) = started(vec![ok(USAGE)]);
        let first = next(&h);
        assert_eq!(first.state, SourceState::Ok);
        *fake.credential.lock().unwrap() = Credential::Expired;
        at.store(T0 + POLL_SECS, Ordering::SeqCst);
        let stale = next(&h);
        assert_eq!(stale.state, SourceState::Stale { since: T0 });
        assert_eq!(stale.notes, [EXPIRED]);
        assert_eq!(stale.windows, first.windows);
        assert_eq!(stale.plan.as_deref(), Some("Max"));
        // No request was made with the expired token.
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
        // Without an earlier value it is an error that names the reason.
        let (_, clock) = fake_clock();
        let (d, fake) = deps(clock, vec![]);
        *fake.credential.lock().unwrap() = Credential::Expired;
        let sp = spawner();
        let h = handle(vec![spawn(d, &sp)], &sp);
        let s = next(&h);
        assert_eq!(s.state, SourceState::Error(EXPIRED.into()));
        assert_eq!(s.notes, [EXPIRED]);
        assert!(s.windows.is_empty());
        assert!(fake.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn unauthorized_marks_token_expired() {
        for code in [401, 403] {
            let (h, fake, at) = started(vec![ok(USAGE), status(code, None)]);
            assert_eq!(next(&h).state, SourceState::Ok);
            at.store(T0 + POLL_SECS, Ordering::SeqCst);
            let s = next(&h);
            assert_eq!(s.state, SourceState::Stale { since: T0 });
            assert_eq!(s.notes, [EXPIRED]);
            // The error body is never shown.
            assert!(!s.notes.iter().any(|n| n.contains("synthetic")));
            assert_eq!(fake.requests.lock().unwrap().len(), 2);
        }
    }

    #[test]
    fn rate_limit_backs_off_and_honours_retry_after() {
        let (h, fake, at) = started(vec![
            HttpResponse::no_connection(),
            HttpResponse::no_connection(),
            status(429, None),
            status(429, Some("600")),
            ok(USAGE),
        ]);
        let count = || fake.requests.lock().unwrap().len();
        let after = |secs: i64| at.store(T0 + secs, Ordering::SeqCst);
        // No connection: 30 s, then 60 s.
        assert_eq!(next(&h).notes, ["Cannot connect (retry in 30s)"]);
        after(29);
        quiet(&h);
        after(30);
        assert_eq!(next(&h).notes, ["Cannot connect (retry in 1m)"]);
        // 429 without Retry-After: the 120 s step is floored to the 180 s gap.
        after(90);
        assert_eq!(next(&h).notes, ["Rate limited (retry in 3m)"]);
        after(269);
        quiet(&h);
        assert_eq!(count(), 3);
        // 429 with Retry-After: 600 s; a manual refresh does not jump it.
        after(270);
        assert_eq!(next(&h).notes, ["Rate limited (retry in 10m)"]);
        h.refresh();
        assert_eq!(
            next(&h).notes.last().map(String::as_str),
            Some("Refresh deferred (next request in 10m)")
        );
        after(869);
        quiet(&h);
        assert_eq!(count(), 4);
        after(870);
        let s = next(&h);
        assert_eq!(s.state, SourceState::Ok);
        assert!(s.notes.is_empty());
        assert_eq!(count(), 5);
    }

    #[test]
    fn manual_refresh_inside_180s_is_deferred() {
        let (h, fake, at) = started(vec![ok(USAGE), ok(USAGE)]);
        assert_eq!(next(&h).state, SourceState::Ok);
        at.store(T0 + 100, Ordering::SeqCst);
        h.refresh();
        let s = next(&h);
        // The value stays as it was, with a note saying when the request runs.
        assert_eq!(s.state, SourceState::Ok);
        assert_eq!(s.notes, ["Refresh deferred (next request in 2m)"]);
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
        at.store(T0 + 179, Ordering::SeqCst);
        quiet(&h);
        at.store(T0 + 180, Ordering::SeqCst);
        let s = next(&h);
        assert_eq!(s.last_updated, Some(T0 + 180));
        assert!(s.notes.is_empty());
        assert_eq!(fake.requests.lock().unwrap().len(), 2);
        // Then the regular poll, 300 s after that.
        at.store(T0 + 479, Ordering::SeqCst);
        quiet(&h);
        assert_eq!(fake.requests.lock().unwrap().len(), 2);
        at.store(T0 + 480, Ordering::SeqCst);
        until("the next poll", || fake.requests.lock().unwrap().len() == 3);
    }

    #[test]
    fn plan_comes_from_subscription_type() {
        let text = br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-SYNTHETIC","expiresAt":1900000000000,"subscriptionType":"max","rateLimitTier":"default_claude_max_20x"}}"#;
        assert_eq!(parse_credential(text, T0), token(Some("Max")));
        let pro = br#"{"claudeAiOauth":{"accessToken":"t","subscriptionType":"pro"}}"#;
        assert!(
            matches!(parse_credential(pro, T0), Credential::Token { plan: Some(p), .. } if p == "Pro")
        );
        let past = br#"{"claudeAiOauth":{"accessToken":"t","expiresAt":1700000000000}}"#;
        assert_eq!(parse_credential(past, T0), Credential::Expired);
        let at_now = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"t","expiresAt":{}}}}}"#,
            T0 * 1000
        );
        assert_eq!(parse_credential(at_now.as_bytes(), T0), Credential::Expired);
        for bad in [
            &br#"{"accessToken":"t"}"#[..],
            br#"{"claudeAiOauth":{"accessToken":""}}"#,
            br#"{"claudeAiOauth":{"accessToken":"a b"}}"#,
            br#"{"claudeAiOauth":{"accessToken":"a\nheader = x"}}"#,
            b"not json",
        ] {
            assert_eq!(parse_credential(bad, T0), Credential::Malformed);
        }
        // The worker labels the source with it.
        let (h, _fake, _) = started(vec![ok(USAGE)]);
        assert_eq!(next(&h).plan.as_deref(), Some("Max"));
        // The token never shows in Debug output.
        assert!(!format!("{:?}", token(None)).contains(TOKEN));
    }

    #[test]
    fn credentials_file_is_never_written() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        let text = include_bytes!("testdata/claude_credentials.json");
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        let before = std::fs::metadata(&path).unwrap();
        let listing = || {
            let mut v: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            v.sort();
            v
        };
        let names = listing();
        // Read through the worker a few cycles, then directly.
        let (at, clock) = fake_clock();
        let (mut d, fake) = deps(clock.clone(), vec![ok(USAGE), ok(USAGE)]);
        let file = path.clone();
        d.credential = Arc::new(move || read_credential_file(&file, clock()));
        let sp = spawner();
        let h = handle(vec![spawn(d, &sp)], &sp);
        assert_eq!(next(&h).plan.as_deref(), Some("Pro"));
        at.store(T0 + POLL_SECS, Ordering::SeqCst);
        next(&h);
        drop(h);
        assert_eq!(
            fake.requests.lock().unwrap()[0].0,
            "sk-ant-oat01-SYNTHETIC-FIXTURE"
        );
        assert!(matches!(
            read_credential_file(&path, T0),
            Credential::Token { .. }
        ));
        assert_eq!(
            read_credential_file(&dir.path().join("absent.json"), T0),
            Credential::Missing
        );
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), text);
        assert_eq!(after.permissions().mode() & 0o777, 0o400);
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
        assert_eq!(after.len(), before.len());
        // Nothing new beside it (no temp file, no lock, no "absent.json").
        assert_eq!(listing(), names);
    }

    #[test]
    fn version_comes_from_the_newest_log_record() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("-synthetic-project");
        std::fs::create_dir_all(project.join("s1/subagents")).unwrap();
        std::fs::write(project.join("old.jsonl"), "{\"version\":\"1.0.0\"}\n").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(
            project.join("s1/subagents/agent-a.jsonl"),
            "{\"version\":\"2.1.5\"}\n{\"type\":\"x\"}\n{\"version\":\"bad version\"}\n",
        )
        .unwrap();
        assert_eq!(version_from_logs(dir.path()).as_deref(), Some("2.1.5"));
        assert_eq!(version_from_logs(&dir.path().join("none")), None);
    }

    #[test]
    fn missing_curl_and_missing_login_are_named() {
        let (_, clock) = fake_clock();
        let (mut d, fake) = deps(clock, vec![]);
        d.http = Arc::new(|| None);
        let sp = spawner();
        let h = handle(vec![spawn(d, &sp)], &sp);
        let s = next(&h);
        assert_eq!(s.state, SourceState::Error("curl not found".into()));
        *fake.credential.lock().unwrap() = Credential::Missing;
        h.refresh();
        let s = next(&h);
        assert_eq!(s.state, SourceState::Unconfigured);
        assert_eq!(
            s.notes,
            ["Not signed in to Claude Code (run claude and log in)"]
        );
    }

    #[test]
    fn refresh_pressed_during_a_request_is_deferred_then_run() {
        let (at, clock) = fake_clock();
        let (mut d, fake) = deps(clock, vec![ok(USAGE), ok(USAGE)]);
        // The first request stays out until the test lets it answer.
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Arc::new(Mutex::new(gate));
        let inner = d.http.clone();
        d.http = Arc::new(move || {
            let real = inner()?;
            let gate = gate.clone();
            Some(Arc::new(move |url: &str, token: &str, ua: &str| {
                let answer = real(url, token, ua);
                let _ = gate.lock().unwrap().recv();
                answer
            }) as HttpFn)
        });
        let sp = spawner();
        let h = handle(vec![spawn(d, &sp)], &sp);
        let count = || fake.requests.lock().unwrap().len();
        until("the first request", || count() == 1);
        at.store(T0 + 1, Ordering::SeqCst);
        h.refresh();
        at.store(T0 + 2, Ordering::SeqCst);
        drop(release);
        let first = next(&h);
        assert_eq!(first.state, SourceState::Ok);
        assert!(first.notes.is_empty());
        // The refresh is not answered by the request already out.
        let deferred = next(&h);
        assert_eq!(deferred.state, SourceState::Ok);
        assert_eq!(deferred.notes, ["Refresh deferred (next request in 3m)"]);
        at.store(T0 + 179, Ordering::SeqCst);
        quiet(&h);
        assert_eq!(count(), 1);
        at.store(T0 + 180, Ordering::SeqCst);
        let s = next(&h);
        assert_eq!(s.last_updated, Some(T0 + 180));
        assert!(s.notes.is_empty());
        assert_eq!(count(), 2);
    }

    #[test]
    fn a_cut_off_request_still_holds_the_180s_gap() {
        // curl failed after connecting: the server may have counted it.
        let (h, fake, at) = started(vec![HttpResponse::interrupted(), ok(USAGE)]);
        let count = || fake.requests.lock().unwrap().len();
        let s = next(&h);
        assert_eq!(s.notes, ["No response (retry in 3m)"]);
        assert_eq!(
            s.state,
            SourceState::Error("No response (retry in 3m)".into())
        );
        // Not at the 30 s connection back-off.
        at.store(T0 + 30, Ordering::SeqCst);
        quiet(&h);
        h.refresh();
        assert_eq!(
            next(&h).notes.last().map(String::as_str),
            Some("Refresh deferred (next request in 3m)")
        );
        at.store(T0 + 179, Ordering::SeqCst);
        quiet(&h);
        assert_eq!(count(), 1);
        at.store(T0 + 180, Ordering::SeqCst);
        assert_eq!(next(&h).state, SourceState::Ok);
        assert_eq!(count(), 2);
        // A connection that never opened backs off 30 s only.
        let (h, fake, at) = started(vec![HttpResponse::no_connection(), ok(USAGE)]);
        assert_eq!(next(&h).notes, ["Cannot connect (retry in 30s)"]);
        at.store(T0 + 30, Ordering::SeqCst);
        assert_eq!(next(&h).state, SourceState::Ok);
        assert_eq!(fake.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn the_gap_runs_on_the_monotonic_clock() {
        let (wall, clock) = fake_clock();
        let (mut d, fake) = deps(clock, vec![ok(USAGE), ok(USAGE)]);
        let elapsed = Arc::new(AtomicI64::new(0));
        let read = elapsed.clone();
        d.mono = Arc::new(move || secs(read.load(Ordering::SeqCst)));
        let sp = spawner();
        let h = handle(vec![spawn(d, &sp)], &sp);
        let count = || fake.requests.lock().unwrap().len();
        assert_eq!(next(&h).state, SourceState::Ok);
        // The wall clock is set an hour ahead; only 10 s have passed.
        wall.store(T0 + 3_600, Ordering::SeqCst);
        elapsed.store(10, Ordering::SeqCst);
        h.refresh();
        assert_eq!(
            next(&h).notes.last().map(String::as_str),
            Some("Refresh deferred (next request in 3m)")
        );
        quiet(&h);
        assert_eq!(count(), 1);
        elapsed.store(179, Ordering::SeqCst);
        quiet(&h);
        assert_eq!(count(), 1);
        elapsed.store(180, Ordering::SeqCst);
        let s = next(&h);
        assert_eq!(s.state, SourceState::Ok);
        // The sample time is still the wall clock's.
        assert_eq!(s.last_updated, Some(T0 + 3_600));
        assert_eq!(count(), 2);
    }

    #[test]
    fn signing_out_drops_the_old_plan() {
        let (h, fake, at) = started(vec![ok(USAGE)]);
        assert_eq!(next(&h).plan.as_deref(), Some("Max"));
        *fake.credential.lock().unwrap() = Credential::Missing;
        at.store(T0 + POLL_SECS, Ordering::SeqCst);
        let s = next(&h);
        assert_eq!(s.state, SourceState::Unconfigured);
        assert_eq!(s.plan, None);
        assert!(s.windows.is_empty());
        // A deferred note on top of it carries no plan either.
        h.refresh();
        let s = next(&h);
        assert_eq!(s.plan, None);
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
    }
}
