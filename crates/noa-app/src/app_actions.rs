//! Small app-level side effects that are not terminal or window state:
//! showing the standard macOS About panel and opening the noa config file in
//! the user's default text editor. Kept out of `app.rs` so the objc2 glue and
//! the config-file bootstrap live in one testable place; the pure config-file
//! template helper is unit-tested without touching AppKit or the filesystem.

use std::path::Path;

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

#[cfg(target_os = "macos")]
fn show_about_macos() {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    // SAFETY: all objects are AppKit runtime objects created and consumed on
    // the main thread (command dispatch runs there), and every selector is the
    // documented NSApplication / NSMutableDictionary API. `NSMutableDictionary`
    // is looked up at runtime rather than linked, matching `notification.rs`'s
    // style, so no extra objc2-foundation feature is needed.
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
            // values ("ApplicationName" / "ApplicationVersion").
            let name = NSString::from_str("Noa");
            let version = NSString::from_str(env!("CARGO_PKG_VERSION"));
            let name_key = NSString::from_str("ApplicationName");
            let version_key = NSString::from_str("ApplicationVersion");
            let _: () = msg_send![options, setObject: &*name, forKey: &*name_key];
            let _: () = msg_send![options, setObject: &*version, forKey: &*version_key];
        }
        // Bring Noa forward first so the panel isn't buried behind other apps.
        let _: () = msg_send![app, activateIgnoringOtherApps: true];
        let _: () = msg_send![app, orderFrontStandardAboutPanelWithOptions: options];
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
}
