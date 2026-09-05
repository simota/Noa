//! Opens a hyperlink/URL target via the macOS `open` command, gated by a
//! scheme allowlist. OSC 8 hyperlink URIs are untrusted terminal output (any
//! program running in the shell can set one), so only a small set of
//! obviously-safe schemes is ever handed to `open`.

/// The Cmd+click open target the hover-link machinery resolved: either an
/// OSC 8 / auto-detected URI (dispatched through the scheme allowlist below)
/// or a filesystem path already resolved by the asynchronous hover probe.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LinkTarget {
    Uri(String),
    Path {
        path: std::path::PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
}

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

/// Open `path` (already resolved to an absolute filesystem path) with the
/// macOS `open` command. Unlike [`open_uri`], there is no scheme to
/// allowlist: the path came from `detect_path_at_column`'s pure
/// text-matching plus the hover-time existence probe, not from untrusted
/// OSC 8 metadata. There is deliberately no click-time `exists()` re-check —
/// a metadata query on a wedged network volume would block the main thread
/// (the same reason the hover probe runs on a worker); a path that vanished
/// between hover and click just makes `open` exit non-zero, logged from the
/// wait below on its own detached thread.
pub fn open_path(
    path: &std::path::Path,
    line: Option<u32>,
    column: Option<u32>,
    editor: noa_config::FileLinkEditor,
) {
    let path = path.to_owned();
    std::thread::spawn(move || {
        if editor != noa_config::FileLinkEditor::Default && path.is_file() {
            let result = std::process::Command::new(editor.as_str())
                .args(editor_arguments(&path, line, column, editor))
                .status();
            match result {
                Ok(status) if status.success() => return,
                Ok(status) => log::warn!(
                    "file-link editor {} exited with {status}; using default application",
                    editor.as_str()
                ),
                Err(err) => log::warn!(
                    "could not launch file-link editor {}: {err}; using default application",
                    editor.as_str()
                ),
            }
        }
        match std::process::Command::new("open").arg(&path).status() {
            Ok(status) if !status.success() => {
                log::warn!("`open` failed for path {} ({status})", path.display());
            }
            Ok(_) => {}
            Err(err) => log::warn!("failed to open path {}: {err}", path.display()),
        }
    });
}

fn editor_arguments(
    path: &std::path::Path,
    line: Option<u32>,
    column: Option<u32>,
    editor: noa_config::FileLinkEditor,
) -> Vec<std::ffi::OsString> {
    let mut args = Vec::new();
    let mut location = path.as_os_str().to_owned();
    if let Some(line) = line {
        if matches!(
            editor,
            noa_config::FileLinkEditor::Code | noa_config::FileLinkEditor::Cursor
        ) {
            args.push("--goto".into());
        }
        location.push(format!(":{}", line.max(1)));
        if let Some(column) = column {
            location.push(format!(":{}", column.max(1)));
        }
    }
    args.push(location);
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_receives_exact_location_as_one_argument() {
        use noa_config::FileLinkEditor;
        use std::ffi::OsString;
        let path = std::path::Path::new("/tmp/日本語 project/$(touch nope);file.rs");
        assert_eq!(
            editor_arguments(path, Some(42), Some(7), FileLinkEditor::Code),
            vec![
                OsString::from("--goto"),
                OsString::from("/tmp/日本語 project/$(touch nope);file.rs:42:7")
            ]
        );
        assert_eq!(
            editor_arguments(path, None, Some(7), FileLinkEditor::Cursor),
            vec![path.as_os_str().to_owned()]
        );
        assert_eq!(
            editor_arguments(path, Some(0), Some(0), FileLinkEditor::Zed),
            vec![OsString::from(
                "/tmp/日本語 project/$(touch nope);file.rs:1:1"
            )]
        );
    }

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
