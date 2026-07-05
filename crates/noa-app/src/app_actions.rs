//! Small app-level side effects that are not terminal or window state:
//! showing the standard macOS About panel and opening the noa config file in
//! the user's default text editor. Kept out of `app.rs` so the objc2 glue and
//! the config-file bootstrap live in one testable place; the pure config-file
//! template helper is unit-tested without touching AppKit or the filesystem.

use std::path::{Path, PathBuf};

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
            let version = NSString::from_str(env!("CARGO_PKG_VERSION"));
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
                let resource_path: Option<Retained<NSString>> =
                    msg_send![bundle, resourcePath];
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
        log::warn!("failed to create config directory {}: {err}", parent.display());
        return;
    }
    if let Err(err) = std::fs::write(path, CONFIG_TEMPLATE) {
        log::warn!("failed to seed config file {}: {err}", path.display());
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
}
