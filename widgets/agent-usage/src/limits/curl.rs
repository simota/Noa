//! The one HTTPS request the widget makes (Claude's usage endpoint), through
//! `/usr/bin/curl`: no HTTP crate is in the locks.
//!
//! The token reaches curl only through its stdin config (`--config -`), so it
//! is in no argument list (`ps`), no environment and no file. `--disable`
//! comes first so a `~/.curlrc` (which could add `--trace` or `--verbose`)
//! is never read. stderr is dropped; the token is never formatted anywhere
//! but the config text written to curl's stdin.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::cli::{self, Cancel, Env, Resolver};

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CURL_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    /// 0 when no status came back (no connection, curl failed or is missing).
    pub status: u16,
    /// The request may have reached the server: curl got past connecting
    /// (a response, a cut-off one, or a failure it cannot place before that).
    pub sent: bool,
    /// The raw `Retry-After` value of the final response.
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Nothing left this machine: no connection, or curl never ran.
    pub fn no_connection() -> HttpResponse {
        HttpResponse {
            status: 0,
            sent: false,
            retry_after: None,
            body: Vec::new(),
        }
    }

    /// Sent, but no usable response came back (cut off, timed out).
    pub fn interrupted() -> HttpResponse {
        HttpResponse {
            sent: true,
            ..HttpResponse::no_connection()
        }
    }
}

/// `(url, token, user_agent) -> response`; injected for tests.
pub type HttpFn = Arc<dyn Fn(&str, &str, &str) -> HttpResponse + Send + Sync>;

/// A quoted curl config value: `\` and `"` escaped, and control characters
/// written as escapes so a value can never end its line and start an option.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0b}' => out.push_str("\\v"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The config curl reads from stdin: the two request headers.
pub fn config_text(token: &str, user_agent: &str) -> String {
    format!(
        "header = {}\nheader = {}\n",
        quote(&format!("Authorization: Bearer {token}")),
        quote(&format!("User-Agent: {user_agent}")),
    )
}

/// curl's arguments: nothing secret, only limits and the output shape.
pub fn args(url: &str) -> Vec<String> {
    [
        "--disable",
        "--silent",
        "--show-error",
        "--max-time",
        "15",
        "--proto",
        "=https",
        "--max-redirs",
        "0",
        "--max-filesize",
        "1048576",
        "--dump-header",
        "-",
        "--write-out",
        "\\n%{http_code}",
        "--config",
        "-",
        url,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Splits curl's stdout (header blocks, body, `\n<code>`) into a response.
/// Interim blocks (`100 Continue`, a proxy's `200 Connection established`)
/// are skipped: the last block belongs to the body.
pub fn parse(raw: &[u8]) -> HttpResponse {
    let (rest, code) = match raw.iter().rposition(|b| *b == b'\n') {
        Some(i) => (&raw[..i], &raw[i + 1..]),
        None => (&raw[..0], raw),
    };
    let code = std::str::from_utf8(code)
        .ok()
        .and_then(|c| c.trim().parse::<u16>().ok());
    let mut rest = rest;
    let mut status_line = None;
    let mut retry_after = None;
    while rest.starts_with(b"HTTP/") {
        let (block, after) = match find(rest, b"\r\n\r\n") {
            Some(i) => (&rest[..i], &rest[i + 4..]),
            None => match find(rest, b"\n\n") {
                Some(i) => (&rest[..i], &rest[i + 2..]),
                None => (rest, &rest[rest.len()..]),
            },
        };
        let text = String::from_utf8_lossy(block);
        let mut lines = text.lines();
        status_line = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse::<u16>().ok());
        retry_after = lines.find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("retry-after")
                .then(|| value.trim().to_string())
        });
        rest = after;
    }
    HttpResponse {
        status: code.or(status_line).unwrap_or(0),
        sent: true,
        retry_after,
        body: rest.to_vec(),
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Seconds to wait from a `Retry-After` value: a delay in seconds or an
/// IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`). At least 1 s.
pub fn retry_after_secs(value: Option<&str>, now: i64) -> Option<i64> {
    let value = value?.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(i64::try_from(secs).unwrap_or(i64::MAX).max(1));
    }
    let mut parts = value.split_whitespace();
    let (_, day, month, year, time, zone) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    );
    if zone != "GMT" {
        return None;
    }
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS.iter().position(|m| *m == month)? + 1;
    let day: u8 = day.parse().ok()?;
    let year: u16 = year.parse().ok()?;
    let at = crate::time::parse_rfc3339(&format!("{year:04}-{month:02}-{day:02}T{time}Z"))?;
    Some((at - now).max(1))
}

/// curl exit codes for failures before the request is written: setup, the
/// proxy or host lookup, the connection and the TLS handshake. Any other
/// failure may come after the server read the request.
const BEFORE_REQUEST: [i32; 24] = [
    1, 2, 3, 4, 5, 6, 7, 26, 35, 43, 45, 48, 49, 53, 54, 58, 59, 60, 77, 83, 90, 91, 97, 98,
];

/// What a curl run says about the request (curl exits 0 once a response
/// came back, whatever its status).
pub fn response_of(result: Result<Vec<u8>, cli::RunFailure>) -> HttpResponse {
    use cli::RunFailure;
    match result {
        Ok(out) => parse(&out),
        Err(RunFailure::NotRun) => HttpResponse::no_connection(),
        Err(RunFailure::Exit(code)) if BEFORE_REQUEST.contains(&code) => {
            HttpResponse::no_connection()
        }
        // Cut off, timed out, too large or killed: it may have been read.
        Err(_) => HttpResponse::interrupted(),
    }
}

/// `/usr/bin/curl`, else `curl` on the resolved PATH. The fallback may ask
/// the login shell: call it on the source's own thread.
pub fn find_curl(resolver: &Resolver) -> Option<(PathBuf, Env)> {
    let system = Path::new("/usr/bin/curl");
    if cli::is_executable(system) {
        return Some((system.to_path_buf(), resolver.inherited().clone()));
    }
    resolver.resolve("curl").map(|r| (r.path, r.env))
}

/// The real request through curl; `None` when curl is not found.
pub fn system_http(resolver: &Resolver, cancel: Cancel) -> Option<HttpFn> {
    let (curl, mut env) = find_curl(resolver)?;
    // Would make curl write TLS session keys (enough to read the token off a
    // capture) or find a config file.
    env.remove("SSLKEYLOGFILE");
    env.remove("CURL_HOME");
    Some(Arc::new(move |url: &str, token: &str, user_agent: &str| {
        let args = args(url);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let config = config_text(token, user_agent);
        response_of(cli::run(
            &curl,
            &args,
            Some(&env),
            Some(config.as_bytes()),
            CURL_TIMEOUT,
            &[0],
            Some(&cancel),
        ))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "sk-ant-oat01-SYNTHETIC-token";

    #[test]
    fn token_is_only_in_the_config_text() {
        let config = config_text(TOKEN, "claude-code/2.1.0");
        assert_eq!(
            config,
            "header = \"Authorization: Bearer sk-ant-oat01-SYNTHETIC-token\"\n\
             header = \"User-Agent: claude-code/2.1.0\"\n"
        );
        assert_eq!(config.matches(TOKEN).count(), 1);
        // A real run through a fake curl: the token arrives on stdin only.
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("seen");
        let fake = cli::tests::script(
            dir.path(),
            "curl",
            &format!(
                "printf '%s\\n' \"$@\" > '{0}.args'\nenv > '{0}.env'\ncat > '{0}.stdin'\n\
                 printf 'HTTP/2 200\\r\\n\\r\\n{{}}\\n200'",
                seen.display()
            ),
        );
        let args = args(USAGE_URL);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = cli::run(
            &fake,
            &args,
            Some(&[("PATH", "/usr/bin:/bin")].into_iter().collect()),
            Some(config.as_bytes()),
            Duration::from_secs(5),
            &[0],
            None,
        )
        .unwrap();
        assert_eq!(parse(&out).status, 200);
        let read =
            |ext: &str| std::fs::read_to_string(format!("{}.{ext}", seen.display())).unwrap();
        assert!(!read("args").contains(TOKEN));
        assert!(!read("env").contains(TOKEN));
        assert!(read("stdin").contains(TOKEN));
    }

    #[test]
    fn args_hold_no_secret() {
        let args = args(USAGE_URL);
        assert_eq!(args[0], "--disable", "~/.curlrc must not be read");
        assert_eq!(args.last().map(String::as_str), Some(USAGE_URL));
        for flag in ["--config", "--proto", "--max-redirs", "--max-time"] {
            assert!(args.iter().any(|a| a == flag), "{flag}");
        }
        let joined = args.join(" ");
        assert!(!joined.contains("Bearer"));
        assert!(!joined.to_ascii_lowercase().contains("authorization"));
        assert!(!joined.contains("--header") && !joined.contains(" -H"));
        let pos = args.iter().position(|a| a == "--config").unwrap();
        assert_eq!(args[pos + 1], "-");
    }

    #[test]
    fn response_splits_headers_body_and_status() {
        let raw = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/2 429\r\ncontent-type: application/json\r\nRetry-After: 120\r\n\r\n{\"error\":\"x\"}\n429";
        let r = parse(raw);
        assert_eq!(r.status, 429);
        assert_eq!(r.retry_after.as_deref(), Some("120"));
        assert_eq!(r.body, b"{\"error\":\"x\"}");
        let ok = parse(b"HTTP/2 200\r\nx: y\r\n\r\n{\"limits\":[]}\n200");
        assert_eq!(ok.status, 200);
        assert_eq!(ok.retry_after, None);
        assert_eq!(ok.body, b"{\"limits\":[]}");
        // Exit 0 without a status: still a request that went out.
        assert_eq!(parse(b"\n000"), HttpResponse::interrupted());
        assert_eq!(parse(b""), HttpResponse::interrupted());
        // A body with blank lines of its own is kept whole.
        let multi = parse(b"HTTP/1.1 200 OK\r\n\r\na\n\nb\n200");
        assert_eq!(multi.body, b"a\n\nb");
        assert_eq!(retry_after_secs(Some("45"), 0), Some(45));
        assert_eq!(retry_after_secs(Some("0"), 0), Some(1));
        let now = crate::time::parse_rfc3339("2026-11-24T08:00:00Z").unwrap();
        assert_eq!(
            retry_after_secs(Some("Tue, 24 Nov 2026 08:10:00 GMT"), now),
            Some(600)
        );
        assert_eq!(retry_after_secs(Some("soon"), now), None);
        assert_eq!(retry_after_secs(None, now), None);
    }

    #[test]
    fn config_escapes_quotes() {
        let config = config_text("a\"b\\c", "ua\"\nheader = \"X: injected");
        assert_eq!(
            config,
            "header = \"Authorization: Bearer a\\\"b\\\\c\"\n\
             header = \"User-Agent: ua\\\"\\nheader = \\\"X: injected\"\n"
        );
        // Exactly two lines, so nothing can add an option.
        assert_eq!(config.lines().count(), 2);
    }

    #[test]
    fn a_failure_after_connecting_counts_as_sent() {
        use cli::RunFailure;
        // Lookup, connection and handshake failures: nothing was sent.
        for code in [6, 7, 35, 60] {
            assert_eq!(
                response_of(Err(RunFailure::Exit(code))),
                HttpResponse::no_connection(),
                "{code}"
            );
        }
        assert!(!response_of(Err(RunFailure::NotRun)).sent);
        // Cut off mid-response, timed out, too large, killed: it may have.
        for failure in [
            RunFailure::Exit(18),
            RunFailure::Exit(28),
            RunFailure::Exit(52),
            RunFailure::Exit(56),
            RunFailure::Exit(92),
            RunFailure::Signal(6),
            RunFailure::TimedOut,
            RunFailure::TooLarge,
        ] {
            let r = response_of(Err(failure));
            assert_eq!((r.status, r.sent), (0, true), "{failure:?}");
        }
        // Through a fake curl that dies mid-body (exit 56, recv error).
        let dir = tempfile::tempdir().unwrap();
        let fake = cli::tests::script(
            dir.path(),
            "curl",
            "cat > /dev/null\nprintf 'HTTP/2 200\\r\\n\\r\\n{\"lim'\nexit 56",
        );
        let run = |tool: &Path| {
            response_of(cli::run(
                tool,
                &[],
                Some(&[("PATH", "/usr/bin:/bin")].into_iter().collect()),
                Some(b"header = \"X: y\"\n"),
                Duration::from_secs(5),
                &[0],
                None,
            ))
        };
        assert_eq!(run(&fake), HttpResponse::interrupted());
        let refused = cli::tests::script(dir.path(), "curl-refused", "cat > /dev/null\nexit 7");
        assert_eq!(run(&refused), HttpResponse::no_connection());
    }
}
