//! Opens a hyperlink/URL target via the macOS `open` command, gated by a
//! scheme allowlist. OSC 8 hyperlink URIs are untrusted terminal output (any
//! program running in the shell can set one), so only a small set of
//! obviously-safe schemes is ever handed to `open`.

const ALLOWED_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// Pure and testable: does `uri` start with an allowed scheme (`scheme:`)?
/// Scheme matching is case-insensitive per RFC 3986; the rest of the URI is
/// untouched.
pub fn is_allowed_uri(uri: &str) -> bool {
    ALLOWED_SCHEMES.iter().any(|scheme| {
        uri.len() > scheme.len()
            && uri.as_bytes()[scheme.len()] == b':'
            && uri[..scheme.len()].eq_ignore_ascii_case(scheme)
    })
}

/// Open `uri` in the user's default handler for it, if its scheme is
/// allowed. Silently refuses (with a log warning) otherwise — this is a
/// best-effort UI action, not a boundary that should ever panic.
pub fn open_uri(uri: &str) {
    if !is_allowed_uri(uri) {
        log::warn!("refusing to open hyperlink with disallowed scheme: {uri}");
        return;
    }
    if let Err(err) = std::process::Command::new("open").arg(uri).spawn() {
        log::warn!("failed to open hyperlink {uri}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_http_https_and_mailto() {
        assert!(is_allowed_uri("http://example.com"));
        assert!(is_allowed_uri("https://example.com/path?q=1"));
        assert!(is_allowed_uri("mailto:someone@example.com"));
    }

    #[test]
    fn allows_scheme_case_insensitively() {
        assert!(is_allowed_uri("HTTPS://example.com"));
        assert!(is_allowed_uri("MailTo:someone@example.com"));
    }

    #[test]
    fn rejects_disallowed_schemes() {
        assert!(!is_allowed_uri("file:///etc/passwd"));
        assert!(!is_allowed_uri("javascript:alert(1)"));
        assert!(!is_allowed_uri("ftp://example.com"));
        assert!(!is_allowed_uri("data:text/html,<script>alert(1)</script>"));
    }

    #[test]
    fn rejects_scheme_lookalikes_without_the_colon_boundary() {
        // "httpx://" starts with "http" but is not the "http" scheme.
        assert!(!is_allowed_uri("httpx://evil.example.com"));
    }

    #[test]
    fn rejects_malformed_or_empty_uris() {
        assert!(!is_allowed_uri(""));
        assert!(!is_allowed_uri("http"));
        assert!(!is_allowed_uri("not a uri at all"));
    }
}
