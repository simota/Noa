//! Opens a hyperlink/URL target via the macOS `open` command, gated by a
//! scheme allowlist. OSC 8 hyperlink URIs are untrusted terminal output (any
//! program running in the shell can set one), so only a small set of
//! obviously-safe schemes is ever handed to `open`.

/// The Cmd+click open target the hover-link machinery resolved: an OSC 8 /
/// auto-detected URI (dispatched through the scheme allowlist below), the
/// local path of an OSC 8 `file://` link, or a filesystem path already
/// resolved by the asynchronous hover probe.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LinkTarget {
    Uri(String),
    FileUri(std::path::PathBuf),
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
    // Wait on a detached thread, mirroring `open_path`: a dropped `Child` is
    // never reaped by std, so a fire-and-forget `spawn()` here left one
    // zombie per opened link for the life of the process (B06, 2026-09
    // audit). `open` returns as soon as the handler is launched, so the
    // thread is short-lived; the main thread must not block on it.
    let uri = uri.to_owned();
    std::thread::spawn(
        move || match std::process::Command::new("open").arg(&uri).status() {
            Ok(status) if !status.success() => {
                log::warn!("`open` failed for hyperlink {uri} ({status})");
            }
            Ok(_) => {}
            Err(err) => log::warn!("failed to open hyperlink {uri}: {err}"),
        },
    );
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
    std::thread::spawn(move || open_path_blocking(&path, line, column, editor));
}

/// Open the local path of an OSC 8 `file://` hyperlink (Ghostty opens these
/// too). Unlike an on-screen path, the target is hidden behind arbitrary
/// link text, so anything `open` would *run* rather than show — an
/// executable, a `.command` script, an app bundle — is refused. The check
/// stats the path, so it runs on the opener thread like [`open_path`]'s.
pub fn open_file_uri(path: std::path::PathBuf, editor: noa_config::FileLinkEditor) {
    std::thread::spawn(move || match openable_file_link(&path) {
        Ok(resolved) => open_path_blocking(&resolved, None, None, editor),
        Err(reason) => log::warn!("not opening file link {}: {reason}", path.display()),
    });
}

/// Extensions macOS `open` executes, installs, or follows instead of
/// displaying (bundles are directories, so this applies to both kinds).
const LAUNCHABLE_EXTENSIONS: [&str; 18] = [
    "app",
    "command",
    "tool",
    "terminal",
    "workflow",
    "action",
    "scpt",
    "scptd",
    "applescript",
    "pkg",
    "mpkg",
    "prefpane",
    "saver",
    "webloc",
    "inetloc",
    "fileloc",
    "url",
    "jar",
];

/// The resolved target of a `file://` link, or why it must not be opened.
/// Judges (and returns, for opening) the resolved path: a symlink without an
/// extension can point at an `.app`, and `open` follows it.
fn openable_file_link(path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let resolved = std::fs::canonicalize(path).map_err(|err| err.to_string())?;
    let metadata = std::fs::metadata(&resolved).map_err(|err| err.to_string())?;
    if launches_when_opened(path, &metadata) || launches_when_opened(&resolved, &metadata) {
        return Err("it would launch rather than open".to_owned());
    }
    // `canonicalize` does not see through a Finder alias, but `open` does —
    // to whatever it references, an app included. Refuse aliases outright.
    if metadata.is_file() && is_finder_alias(&resolved) {
        return Err("it is a Finder alias".to_owned());
    }
    Ok(resolved)
}

/// A Finder alias: the `kIsAlias` Finder flag, or bookmark-alias data
/// (`book\0\0\0\0mark`) for an alias that lost its Finder info.
fn is_finder_alias(path: &std::path::Path) -> bool {
    use std::io::Read;

    let mut magic = [0u8; 12];
    has_finder_alias_flag(path)
        || std::fs::File::open(path)
            .and_then(|mut file| file.read_exact(&mut magic))
            .is_ok_and(|()| &magic == b"book\0\0\0\0mark")
}

/// The `kIsAlias` bit of the `com.apple.FinderInfo` attribute.
#[cfg(target_os = "macos")]
fn has_finder_alias_flag(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    const K_IS_ALIAS: u16 = 0x8000;
    let mut finder_info = [0u8; 32];
    std::ffi::CString::new(path.as_os_str().as_bytes()).is_ok_and(|c_path| {
        // SAFETY: both pointers are valid for the call; the buffer length is
        // passed, and the attribute name is a NUL-terminated literal.
        let len = unsafe {
            libc::getxattr(
                c_path.as_ptr(),
                c"com.apple.FinderInfo".as_ptr(),
                finder_info.as_mut_ptr().cast(),
                finder_info.len(),
                0,
                0,
            )
        };
        len >= 10 && u16::from_be_bytes([finder_info[8], finder_info[9]]) & K_IS_ALIAS != 0
    })
}

/// Finder info exists only on macOS; elsewhere an alias file can only be
/// recognized by its bookmark data.
#[cfg(not(target_os = "macos"))]
fn has_finder_alias_flag(_path: &std::path::Path) -> bool {
    false
}

fn launches_when_opened(path: &std::path::Path, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let launchable_extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            LAUNCHABLE_EXTENSIONS
                .iter()
                .any(|launchable| ext.eq_ignore_ascii_case(launchable))
        });
    launchable_extension || (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn open_path_blocking(
    path: &std::path::Path,
    line: Option<u32>,
    column: Option<u32>,
    editor: noa_config::FileLinkEditor,
) {
    if editor != noa_config::FileLinkEditor::Default && path.is_file() {
        let result = std::process::Command::new(editor.as_str())
            .args(editor_arguments(path, line, column, editor))
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
    match std::process::Command::new("open").arg(path).status() {
        Ok(status) if !status.success() => {
            log::warn!("`open` failed for path {} ({status})", path.display());
        }
        Ok(_) => {}
        Err(err) => log::warn!("failed to open path {}: {err}", path.display()),
    }
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
    fn file_links_refuse_what_open_would_run() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("noa-link-open-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Evil.app")).unwrap();
        let check = |name: &str, mode: u32| {
            let path = dir.join(name);
            if !path.exists() {
                std::fs::write(&path, b"x").unwrap();
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            launches_when_opened(&path, &std::fs::metadata(&path).unwrap())
        };
        assert!(!check("notes.md", 0o644));
        assert!(check("run.command", 0o644));
        assert!(check("Run.COMMAND", 0o644));
        assert!(check("payload", 0o755));
        let bundle = dir.join("Evil.app");
        assert!(launches_when_opened(
            &bundle,
            &std::fs::metadata(&bundle).unwrap()
        ));
        assert!(!launches_when_opened(
            &dir,
            &std::fs::metadata(&dir).unwrap()
        ));

        // A Finder alias opens whatever it references, so it is refused.
        let alias = dir.join("alias");
        std::fs::write(&alias, b"book\0\0\0\0mark\0\0\0\0").unwrap();
        assert!(openable_file_link(&alias).is_err());
        #[cfg(target_os = "macos")]
        {
            let flagged = dir.join("flagged");
            std::fs::write(&flagged, b"x").unwrap();
            let mut finder_info = [0u8; 32];
            finder_info[8] = 0x80;
            let c_path = std::ffi::CString::new(flagged.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: valid NUL-terminated path/name and an in-bounds buffer.
            let set = unsafe {
                libc::setxattr(
                    c_path.as_ptr(),
                    c"com.apple.FinderInfo".as_ptr(),
                    finder_info.as_ptr().cast(),
                    finder_info.len(),
                    0,
                    0,
                )
            };
            assert_eq!(set, 0);
            assert!(openable_file_link(&flagged).is_err());
        }

        // A bare-named symlink to a bundle is judged by its target.
        let link = dir.join("innocent");
        std::os::unix::fs::symlink(dir.join("Evil.app"), &link).unwrap();
        assert!(openable_file_link(&link).is_err());
        let doc_link = dir.join("doc");
        std::os::unix::fs::symlink(dir.join("notes.md"), &doc_link).unwrap();
        assert_eq!(
            openable_file_link(&doc_link).unwrap(),
            std::fs::canonicalize(dir.join("notes.md")).unwrap()
        );
        std::fs::remove_dir_all(&dir).unwrap();
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
