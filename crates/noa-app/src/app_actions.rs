//! Small app-level side effects that are not terminal or window state:
//! showing the standard macOS About panel and opening the noa config file in
//! the user's default text editor. Kept out of `app.rs` so the objc2 glue and
//! the config-file bootstrap live in one testable place; the pure config-file
//! template helper is unit-tested without touching AppKit or the filesystem.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The seed written to a missing config file the first time the user opens
/// Preferences, so the editor always has something to show (an empty file
/// reads as "nothing happened"). Comments only — every setting stays at its
/// built-in default until the user uncomments one.
pub(crate) const CONFIG_TEMPLATE: &str = "\
# Noa configuration
#
# Noa reads Ghostty-style `key = value` lines. Lines starting with `#` are
# comments. Remove the `#` and set a value to override a default.
#
# font-size = 15
# sidebar-enabled = true
# sidebar-width = 360
# sidebar-font-size = 11.5
# sidebar-preview-lines = 3
# confirm-quit = true
#
# Raise unreadably low-contrast text toward this WCAG contrast ratio
# (1.0 = leave theme colors untouched; 1.1-3.0 are reasonable floors).
# minimum-contrast = 1.1
#
# Show a `cols × rows` toast during a live resize: after-first | always | never
# resize-overlay = after-first
#
# Flash the window briefly when its terminal rings the bell.
# visual-bell = true
#
# Play the system alert sound when a terminal rings the bell. The optional
# focus gate keeps it quiet while you are looking at the target window; the
# Dock bounce is only requested for unfocused target windows.
# audible-bell = true
# audible-bell-when-unfocused = true
# audible-bell-dock-bounce = true
";

/// Show the standard macOS About panel for Noa, seeded with the app name and
/// the crate version (a plain `cargo run` has no Info.plist to read them
/// from). A no-op off macOS.
pub(crate) fn show_about() {
    #[cfg(target_os = "macos")]
    show_about_macos();
    #[cfg(not(target_os = "macos"))]
    log::info!("About Noa (v{})", env!("CARGO_PKG_VERSION"));
}

/// The About panel's version string: `CARGO_PKG_VERSION` extended with the
/// build.rs-embedded git hash and UTC build date when both are available
/// (R-6), else the plain crate version (R-5 — a non-git build environment).
/// This extended form is About-panel-only; `cli.rs`'s `--version` and
/// `noa-grid`'s XTVERSION/DA report both keep using the bare
/// `CARGO_PKG_VERSION` (NFR-2).
fn version_string() -> String {
    compose_version(
        env!("CARGO_PKG_VERSION"),
        env!("NOA_GIT_HASH"),
        env!("NOA_BUILD_DATE"),
    )
}

/// Pure formatting logic behind [`version_string`], split out because
/// `env!()` is compile-time and can't be varied from a unit test.
fn compose_version(version: &str, git_hash: &str, build_date: &str) -> String {
    if git_hash.is_empty() || build_date.is_empty() {
        return version.to_string();
    }
    format!("{version} ({git_hash}, {build_date})")
}

/// Resolve the `noa.icns` path for the About panel from an ordered list of
/// candidates: the first one that exists on disk wins (R-1). `None` when none
/// of them exist, so the caller can fall back to the AppKit standard icon
/// instead of panicking (R-2).
pub(crate) fn icon_path_from_candidates(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.exists()).cloned()
}

#[cfg(target_os = "macos")]
fn show_about_macos() {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    // SAFETY: all objects are AppKit runtime objects created and consumed on
    // the main thread (command dispatch runs there), and every selector is the
    // documented NSApplication / NSMutableDictionary / NSBundle / NSImage API.
    // `NSMutableDictionary` and `NSBundle`/`NSImage` are looked up at runtime
    // rather than linked, matching `notification.rs`'s style, so no extra
    // objc2-foundation/objc2-app-kit feature is needed.
    unsafe {
        let (Some(app_class), Some(dict_class)) = (
            AnyClass::get(c"NSApplication"),
            AnyClass::get(c"NSMutableDictionary"),
        ) else {
            return;
        };
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return;
        }
        let options: *mut AnyObject = msg_send![dict_class, dictionary];
        if !options.is_null() {
            // The option keys are the documented NSAboutPanelOption* string
            // values ("ApplicationName" / "ApplicationVersion" / "ApplicationIcon").
            let name = NSString::from_str("Noa");
            let version = NSString::from_str(&version_string());
            let name_key = NSString::from_str("ApplicationName");
            let version_key = NSString::from_str("ApplicationVersion");
            let _: () = msg_send![options, setObject: &*name, forKey: &*name_key];
            let _: () = msg_send![options, setObject: &*version, forKey: &*version_key];

            if let Some(icon_path) = icon_path_from_candidates(&icon_candidates())
                && let Some(icon_class) = AnyClass::get(c"NSImage")
            {
                let icon_path_str = NSString::from_str(&icon_path.to_string_lossy());
                let alloc: *mut AnyObject = msg_send![icon_class, alloc];
                let image: *mut AnyObject =
                    msg_send![alloc, initWithContentsOfFile: &*icon_path_str];
                if !image.is_null() {
                    let icon_key = NSString::from_str("ApplicationIcon");
                    let _: () = msg_send![options, setObject: image, forKey: &*icon_key];
                }
            }
        }
        // Bring Noa forward first so the panel isn't buried behind other apps.
        let _: () = msg_send![app, activateIgnoringOtherApps: true];
        let _: () = msg_send![app, orderFrontStandardAboutPanelWithOptions: options];
    }

    /// The two-step icon lookup from R-1: the bundled resource path first
    /// (a `.app` launch via `bundle-macos.sh`), then the workspace `assets/`
    /// directory as the `cargo run` fallback.
    unsafe fn icon_candidates() -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(bundle_class) = AnyClass::get(c"NSBundle") {
            let bundle: *mut AnyObject = msg_send![bundle_class, mainBundle];
            if !bundle.is_null() {
                let resource_path: Option<Retained<NSString>> = msg_send![bundle, resourcePath];
                if let Some(resource_path) = resource_path {
                    candidates.push(PathBuf::from(resource_path.to_string()).join("noa.icns"));
                }
            }
        }
        candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/noa.icns"));
        candidates
    }
}

/// Open the noa config file in the user's default text editor, creating it
/// from [`CONFIG_TEMPLATE`] first if it does not yet exist so the editor has
/// content to show. Best-effort: every failure is logged, none panics.
pub(crate) fn open_config_file() {
    let Some(path) = noa_config::default_config_path() else {
        log::warn!("could not resolve the noa config path");
        return;
    };
    ensure_config_file(&path);
    // `open -t` forces the default *text* editor; the config file is
    // extensionless, so a plain `open` could route it to an arbitrary handler.
    if let Err(err) = std::process::Command::new("open")
        .arg("-t")
        .arg(&path)
        .spawn()
    {
        log::warn!("failed to open config file {}: {err}", path.display());
    }
}

/// Create the config file (and its parent directory) seeded with the template
/// when it is absent. A no-op when the file already exists.
fn ensure_config_file(path: &Path) {
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        log::warn!(
            "failed to create config directory {}: {err}",
            parent.display()
        );
        return;
    }
    if let Err(err) = std::fs::write(path, CONFIG_TEMPLATE) {
        log::warn!("failed to seed config file {}: {err}", path.display());
    }
}

pub(crate) fn write_scrollback_temp_file(text: &str) -> io::Result<PathBuf> {
    write_scrollback_temp_file_in_dir(text, &std::env::temp_dir())
}

pub(crate) fn export_scrollback_to_file(
    text: &str,
    selected_path: Option<&Path>,
) -> io::Result<Option<PathBuf>> {
    let Some(path) = selected_path else {
        return Ok(None);
    };
    std::fs::write(path, text.as_bytes())?;
    Ok(Some(path.to_path_buf()))
}

#[cfg(target_os = "macos")]
pub(crate) fn choose_scrollback_export_path() -> io::Result<Option<PathBuf>> {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    const NS_MODAL_RESPONSE_OK: isize = 1;

    // SAFETY: command dispatch runs on the main thread. All selectors are
    // documented NSSavePanel/NSURL APIs, and returned objects are nil-checked.
    unsafe {
        let panel_class = AnyClass::get(c"NSSavePanel")
            .ok_or_else(|| io::Error::other("macOS save panel is unavailable"))?;
        let panel: *mut AnyObject = msg_send![panel_class, savePanel];
        if panel.is_null() {
            return Err(io::Error::other("could not create the macOS save panel"));
        }

        let default_name = NSString::from_str("noa-scrollback.txt");
        let _: () = msg_send![panel, setNameFieldStringValue: &*default_name];
        let _: () = msg_send![panel, setCanCreateDirectories: true];
        let response: isize = msg_send![panel, runModal];
        if response != NS_MODAL_RESPONSE_OK {
            return Ok(None);
        }

        let url: *mut AnyObject = msg_send![panel, URL];
        if url.is_null() {
            return Err(io::Error::other("the save panel returned no destination"));
        }
        let path: Option<Retained<NSString>> = msg_send![url, path];
        let Some(path) = path.filter(|path| !path.is_empty()) else {
            return Err(io::Error::other(
                "the save panel returned an empty destination",
            ));
        };
        Ok(Some(PathBuf::from(path.to_string())))
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn choose_scrollback_export_path() -> io::Result<Option<PathBuf>> {
    Ok(None)
}

fn write_scrollback_temp_file_in_dir(text: &str, dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..100 {
        let path = dir.join(format!(
            "noa-scrollback-{}-{stamp}-{attempt}.txt",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique scrollback export file",
    ))
}

pub(crate) fn pager_shell_command(path: &Path) -> String {
    pager_shell_command_from_parts(path, pager_command())
}

fn pager_shell_command_from_parts(path: &Path, pager: PagerCommand) -> String {
    let mut parts = Vec::with_capacity(1 + pager.args.len() + 2);
    parts.push(crate::clipboard::shell_escape(&pager.program));
    parts.extend(
        pager
            .args
            .iter()
            .map(|arg| crate::clipboard::shell_escape(arg)),
    );
    parts.push("<".to_string());
    parts.push(crate::clipboard::shell_escape(&path.to_string_lossy()));
    format!("{}\n", parts.join(" "))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PagerCommand {
    program: String,
    args: Vec<String>,
}

fn pager_command() -> PagerCommand {
    pager_command_from_env_value(std::env::var("PAGER").ok().as_deref())
}

fn pager_command_from_env_value(value: Option<&str>) -> PagerCommand {
    let mut parts = value
        .unwrap_or("")
        .split_whitespace()
        .filter(|part| !part.is_empty());
    let Some(program) = parts.next() else {
        return PagerCommand {
            program: "less".to_string(),
            args: vec!["-R".to_string()],
        };
    };
    PagerCommand {
        program: program.to_string(),
        args: parts.map(str::to_string).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_template_is_comment_only_so_it_changes_no_defaults() {
        for line in CONFIG_TEMPLATE.lines() {
            let trimmed = line.trim();
            assert!(
                trimmed.is_empty() || trimmed.starts_with('#'),
                "template line is not a comment: {line:?}"
            );
        }
    }

    #[test]
    fn ensure_config_file_seeds_a_missing_file_then_leaves_it_untouched() {
        let dir = std::env::temp_dir().join(format!("noa-cfg-test-{}", std::process::id()));
        let path = dir.join("config");
        let _ = std::fs::remove_dir_all(&dir);

        ensure_config_file(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG_TEMPLATE);

        // A second call must not clobber user edits.
        std::fs::write(&path, "font-size = 20\n").unwrap();
        ensure_config_file(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "font-size = 20\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compose_version_appends_hash_and_date_when_both_present() {
        assert_eq!(
            compose_version("0.1.0", "a1b2c3d", "2026-07-05"),
            "0.1.0 (a1b2c3d, 2026-07-05)"
        );
    }

    #[test]
    fn compose_version_falls_back_to_the_plain_version_when_either_field_is_empty() {
        assert_eq!(compose_version("0.1.0", "", "2026-07-05"), "0.1.0");
        assert_eq!(compose_version("0.1.0", "a1b2c3d", ""), "0.1.0");
        assert_eq!(compose_version("0.1.0", "", ""), "0.1.0");
    }

    #[test]
    fn icon_path_from_candidates_returns_none_when_all_paths_are_missing() {
        let candidates = vec![
            PathBuf::from("/nonexistent/noa-about-test/noa.icns"),
            PathBuf::from("/also/missing/noa.icns"),
        ];
        assert_eq!(icon_path_from_candidates(&candidates), None);
    }

    #[test]
    fn icon_path_from_candidates_returns_the_first_existing_path() {
        // CARGO_MANIFEST_DIR always exists at test time, so it stands in for
        // a real icon file without needing a tempdir fixture.
        let existing = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let candidates = vec![PathBuf::from("/nonexistent/noa.icns"), existing.clone()];
        assert_eq!(icon_path_from_candidates(&candidates), Some(existing));
    }

    #[test]
    fn scrollback_temp_file_is_written_with_requested_text() {
        let dir = std::env::temp_dir().join(format!(
            "noa-scrollback-export-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let path = write_scrollback_temp_file_in_dir("one\ntwo\n", &dir).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scrollback_export_uses_selected_path_and_preserves_exact_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "noa-scrollback-selected-export-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let selected_path = dir.join("chosen-scrollback.txt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&selected_path, b"stale content that must be truncated").unwrap();
        let scrollback = "first\r\nsecond\nUnicode: \u{7d42}\n";

        let exported = export_scrollback_to_file(scrollback, Some(&selected_path)).unwrap();

        assert_eq!(exported.as_deref(), Some(selected_path.as_path()));
        assert_eq!(
            std::fs::read(&selected_path).unwrap(),
            scrollback.as_bytes()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_scrollback_export_creates_no_file() {
        let exported = export_scrollback_to_file("must not be written", None).unwrap();

        assert_eq!(exported, None);
    }

    #[test]
    fn pager_command_defaults_to_less_r_and_splits_simple_env_values() {
        assert_eq!(
            pager_command_from_env_value(None),
            PagerCommand {
                program: "less".to_string(),
                args: vec!["-R".to_string()],
            }
        );
        assert_eq!(
            pager_command_from_env_value(Some("more -d")),
            PagerCommand {
                program: "more".to_string(),
                args: vec!["-d".to_string()],
            }
        );
        assert_eq!(
            pager_command_from_env_value(Some("   ")),
            PagerCommand {
                program: "less".to_string(),
                args: vec!["-R".to_string()],
            }
        );
    }

    #[test]
    fn pager_shell_command_pipes_temp_file_to_pager() {
        let command = pager_shell_command_from_parts(
            Path::new("/tmp/noa scrollback's.txt"),
            PagerCommand {
                program: "less".to_string(),
                args: vec!["-R".to_string()],
            },
        );

        assert_eq!(command, "less -R < '/tmp/noa scrollback'\\''s.txt'\n");
    }
}
