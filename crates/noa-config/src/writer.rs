//! Surgical writer for the Ghostty-style `key = value` config format:
//! updates a small set of keys in place while leaving every other byte of
//! the source text untouched (comments, unknown keys, blank lines, and line
//! order). This is the inverse of `import.rs`'s `build_import_output` — that
//! module turns *foreign* text into a noa config; this module turns an
//! *existing* noa config into an updated one — and follows the same split:
//! a pure string-in/string-out function plus a thin I/O wrapper.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use crate::parser::{Directive, parse_directives};

/// Applies `updates` (key, value pairs) to `original`, returning the updated
/// config text.
///
/// - A key already present is rewritten in place as `key = value`; if the
///   key occurs on multiple lines (duplicate directives), only the **last**
///   occurrence is replaced; earlier occurrences are left untouched. This
///   preserves last-wins scalar resolution and intentional repeatable entries.
/// - The `font-family*` list keys are the exception: the parser
///   accumulates every line into an ordered family stack, and the settings
///   panel edits the *primary* family (the head of that stack). A non-empty
///   update therefore replaces the **primary slot** — the first occurrence
///   after the last empty-valued (list-reset) line — so fallbacks after it
///   survive (B05, 2026-09 audit). An empty update is the list reset itself
///   and keeps last-occurrence placement, with a trailing reset appended
///   when a later include could add more entries (B06).
/// - A key absent from `original` is appended as a new `key = value` line at
///   the end.
/// - If a `config-file` include directive appears *after* a scalar key's last
///   occurrence, the key is additionally appended at the end so the
///   included file cannot shadow the new value (includes splice in at the
///   directive's position, so only a trailing line is guaranteed to win).
/// - Every other line (comments, unknown keys, blank lines, other keys, and
///   the original line order) is preserved byte-for-byte.
pub fn apply_updates(original: &str, updates: &[(String, String)]) -> String {
    if updates.is_empty() {
        return original.to_string();
    }

    let directives = parse_directives(original);
    let mut replacements: HashMap<usize, String> = HashMap::new();
    let mut appended: Vec<&(String, String)> = Vec::new();

    // The reader splices an included file's directives in at the point of
    // its `config-file` line, so an include *after* the key's last
    // occurrence can still shadow an in-place rewrite. In that case the new
    // scalar value or font-family reset is also appended after every include.
    // Non-empty repeatable values accumulate instead, so appending them would
    // leave stale entries in the list on subsequent saves.
    let last_include_line = directives
        .iter()
        .filter(|directive| directive.key == "config-file")
        .map(|directive| directive.line)
        .max();

    for update @ (key, value) in updates {
        let target = if is_font_family_key(key) && !value.is_empty() {
            primary_family_slot(&directives, key)
        } else {
            directives
                .iter()
                .rev()
                .find(|directive| &directive.key == key)
        };
        match target {
            Some(directive) => {
                replacements.insert(directive.line, format!("{key} = {value}"));
                if (!is_repeatable_key(key) || (is_font_family_key(key) && value.is_empty()))
                    && last_include_line.is_some_and(|include| include > directive.line)
                {
                    appended.push(update);
                }
            }
            None => appended.push(update),
        }
    }

    let lines = split_lines_preserving_terminators(original);
    let append_terminator = dominant_terminator(&lines);

    let mut output = String::new();
    for (index, (content, terminator)) in lines.iter().enumerate() {
        match replacements.remove(&(index + 1)) {
            Some(replacement) => output.push_str(&replacement),
            None => output.push_str(content),
        }
        output.push_str(terminator);
    }

    if !appended.is_empty() {
        // The last existing line may carry no terminator at all (`original`
        // had no trailing newline) — give it the file's dominant one so the
        // first appended key still lands on its own line.
        if lines
            .last()
            .is_some_and(|(_, terminator)| terminator.is_empty())
        {
            output.push_str(append_terminator);
        }
        for (key, value) in &appended {
            output.push_str(&format!("{key} = {value}"));
            output.push_str(append_terminator);
        }
    }

    output
}

fn is_font_family_key(key: &str) -> bool {
    matches!(
        key,
        "font-family" | "font-family-bold" | "font-family-italic" | "font-family-bold-italic"
    )
}

/// The line holding the *primary* (first effective) entry of a
/// `font-family*` stack: the first occurrence of `key` after its last
/// empty-valued line, since an empty value resets the list in the parser.
/// `None` when the file has no effective entry (never set, or reset last).
fn primary_family_slot<'a>(directives: &'a [Directive], key: &str) -> Option<&'a Directive> {
    let last_reset = directives
        .iter()
        .rposition(|directive| directive.key == key && is_empty_value(directive));
    directives
        .iter()
        .skip(last_reset.map_or(0, |index| index + 1))
        .find(|directive| directive.key == key && !is_empty_value(directive))
}

fn is_empty_value(directive: &Directive) -> bool {
    directive.value.as_deref().is_none_or(str::is_empty)
}

fn is_repeatable_key(key: &str) -> bool {
    matches!(
        key,
        "font-family"
            | "font-family-bold"
            | "font-family-italic"
            | "font-family-bold-italic"
            | "font-feature"
            | "font-variation"
            | "font-variation-bold"
            | "font-variation-italic"
            | "font-variation-bold-italic"
            | "palette"
            | "keybind"
            | "config-file"
    )
}

/// Splits `text` into `(content, terminator)` pairs, where `terminator` is
/// `"\r\n"`, `"\n"`, or `""` (only the final line, when `text` has no
/// trailing newline). Unlike [`str::lines`], the terminator survives per
/// line so [`apply_updates`] can write each untouched line back with its
/// original ending — NFR-5 requires a CRLF source file to round-trip as
/// CRLF, not be silently normalized to LF.
fn split_lines_preserving_terminators(text: &str) -> Vec<(&str, &str)> {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find('\n') {
            Some(idx) => {
                let (line, remainder) = rest.split_at(idx);
                let remainder = &remainder[1..];
                match line.strip_suffix('\r') {
                    Some(stripped) => lines.push((stripped, "\r\n")),
                    None => lines.push((line, "\n")),
                }
                rest = remainder;
            }
            None => {
                lines.push((rest, ""));
                rest = "";
            }
        }
    }
    lines
}

/// The majority line terminator among `lines`' terminated lines, used only
/// to end newly *appended* keys — an untouched existing line's terminator
/// always comes from its own original byte, never from this. Ties (and a
/// file with no terminated lines at all) fall back to `"\n"`.
fn dominant_terminator(lines: &[(&str, &str)]) -> &'static str {
    let crlf = lines.iter().filter(|(_, term)| *term == "\r\n").count();
    let lf = lines.iter().filter(|(_, term)| *term == "\n").count();
    if crlf > lf { "\r\n" } else { "\n" }
}

/// Reads the config at `path` (an absent file is treated as empty), applies
/// `updates` via [`apply_updates`], and writes the result back atomically
/// (temp file in the same directory + `rename`), so a crash mid-write cannot
/// leave a truncated or partially-written config (NFR-3). Mirrors the
/// write pattern in `noa-app/src/session.rs`'s `save`.
///
/// If `path` is a symlink (e.g. a dotfiles-managed config), the write
/// targets the symlink's resolved destination so the symlink itself is
/// preserved rather than being replaced by a regular file — including a
/// *dangling* link whose target file does not exist yet, which is written
/// (with its parent directory created) instead of being clobbered (B08,
/// 2026-09 audit). A symlink cycle is an error rather than a replacement.
pub fn write_config_updates(path: &Path, updates: &[(String, String)]) -> io::Result<()> {
    let original = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };

    let updated = apply_updates(&original, updates);
    let updated = repair_font_family_primaries(path, updated, updates);

    let target = resolve_symlinks(path)?;
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "config path has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;

    // Create the temp file with a unique name so two concurrent writers never
    // clobber each other's staging file, and 0600 so a config containing
    // e.g. `server-token` is never briefly world-readable via the umask
    // default. The existing file's mode (if any) is carried over before the
    // rename so a user-tightened (0600) or user-loosened (0644) config keeps
    // its permissions across a save.
    let existing_mode = fs::metadata(&target).ok().map(|m| m.permissions());
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config".to_string()),
        std::process::id()
    ));
    let write_result = write_private(&tmp, updated.as_bytes()).and_then(|()| {
        if let Some(perms) = existing_mode {
            fs::set_permissions(&tmp, perms)?;
        }
        fs::rename(&tmp, &target)
    });
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

/// [`apply_updates`] only sees the primary file, but the parser splices
/// `config-file` includes in at their directive's position and accumulates
/// every `font-family*` line into one ordered stack. A family contributed by
/// an include that precedes the rewritten slot — or by an include standing
/// in for a slot the primary file never had — therefore still heads the
/// effective list after a save, and the panel's "primary font" change is
/// silently ineffective (N04). Re-parse the would-be file with includes
/// expanded and, when the requested family did not land in front, rewrite
/// the key as a list reset followed by the new primary and every surviving
/// fallback (in effective order, deduplicated), all trailing the last
/// include so nothing can shadow them.
fn repair_font_family_primaries(
    path: &Path,
    mut updated: String,
    updates: &[(String, String)],
) -> String {
    for (key, value) in updates {
        if !is_font_family_key(key) || value.is_empty() {
            continue;
        }
        let (overrides, _) = crate::parse_overrides(path, &updated);
        let families = font_families_for_key(&overrides.font, key);
        if families.first() == Some(value) {
            continue;
        }
        // The reset replaces the value line `apply_updates` just placed (the
        // key's last occurrence) and, when an include follows it, is appended
        // again after that include — so the primary + fallbacks appended
        // below start from an empty list in every reload order.
        updated = apply_updates(&updated, &[(key.clone(), String::new())]);
        let mut lines = vec![(key.clone(), value.clone())];
        lines.extend(
            families
                .iter()
                .filter(|family| *family != value)
                .map(|family| (key.clone(), family.clone())),
        );
        updated = append_directives(&updated, &lines);
    }
    updated
}

fn font_families_for_key<'a>(font: &'a crate::FontConfig, key: &str) -> &'a [String] {
    match key {
        "font-family-bold" => &font.families_bold,
        "font-family-italic" => &font.families_italic,
        "font-family-bold-italic" => &font.families_bold_italic,
        _ => &font.families,
    }
}

/// Append `lines` as `key = value` directives at the end of `text`, on the
/// file's dominant line terminator (same rule as [`apply_updates`]).
fn append_directives(text: &str, lines: &[(String, String)]) -> String {
    let existing = split_lines_preserving_terminators(text);
    let terminator = dominant_terminator(&existing);
    let mut output = text.to_string();
    if existing
        .last()
        .is_some_and(|(_, terminator)| terminator.is_empty())
    {
        output.push_str(terminator);
    }
    for (key, value) in lines {
        output.push_str(&format!("{key} = {value}"));
        output.push_str(terminator);
    }
    output
}

/// Upper bound on symlink hops before `resolve_symlinks` reports a cycle
/// (Linux's `MAXSYMLINKS` — `fs::canonicalize` would fail at the same depth).
const MAX_SYMLINK_HOPS: usize = 40;

/// Follows `path` through every symlink to the final non-link path, which
/// may not exist. Unlike `fs::canonicalize`, this works for a dangling
/// link (the target's parent need not exist either) and never resolves the
/// intermediate directories, so the returned path stays usable for a
/// sibling temp file + `rename`. Relative link targets resolve against the
/// link's own directory, per symlink semantics.
fn resolve_symlinks(path: &Path) -> io::Result<std::path::PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(current),
            Err(err) => return Err(err),
        };
        if !metadata.file_type().is_symlink() {
            return Ok(current);
        }
        let link_target = fs::read_link(&current)?;
        current = if link_target.is_absolute() {
            link_target
        } else {
            current
                .parent()
                .map(|parent| parent.join(&link_target))
                .unwrap_or(link_target)
        };
    }
    Err(io::Error::other(format!(
        "config path {} exceeds {MAX_SYMLINK_HOPS} symlink hops (cycle?)",
        path.display()
    )))
}

/// Creates `path` (truncating any stale leftover) with owner-only
/// permissions on unix and writes `contents` to it.
fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noa-config-writer-{name}-{}", std::process::id()))
    }

    #[test]
    fn round_trip_preserves_untouched_lines() {
        let original = "\
# a leading comment
window-width = 100
bogus-key = x
font-size = 14

# trailing comment
theme = 3024 Day
";

        let output = apply_updates(original, &[("font-size".to_string(), "18".to_string())]);

        let expected = "\
# a leading comment
window-width = 100
bogus-key = x
font-size = 18

# trailing comment
theme = 3024 Day
";
        assert_eq!(output, expected);
    }

    #[test]
    fn round_trip_preserves_untouched_crlf_lines() {
        let original = "\
# a leading comment\r
window-width = 100\r
bogus-key = x\r
font-size = 14\r
\r
# trailing comment\r
theme = 3024 Day\r
";

        let output = apply_updates(original, &[("font-size".to_string(), "18".to_string())]);

        let expected = "\
# a leading comment\r
window-width = 100\r
bogus-key = x\r
font-size = 18\r
\r
# trailing comment\r
theme = 3024 Day\r
";
        assert_eq!(output, expected);
    }

    #[test]
    fn duplicate_key_replaces_only_last_occurrence() {
        let original = "font-size = 12\nfont-size = 14\n";

        let output = apply_updates(original, &[("font-size".to_string(), "16".to_string())]);

        assert_eq!(output, "font-size = 12\nfont-size = 16\n");
    }

    #[test]
    fn key_shadowed_by_trailing_include_is_also_appended() {
        let original = "font-size = 14\nconfig-file = child.conf\n";

        let output = apply_updates(original, &[("font-size".to_string(), "22".to_string())]);

        assert_eq!(
            output,
            "font-size = 22\nconfig-file = child.conf\nfont-size = 22\n"
        );
    }

    #[test]
    fn repeatable_keys_are_not_duplicated_after_include() {
        for (key, first, second) in [
            ("font-family", "Menlo", "Monaco"),
            ("font-family-bold", "Menlo", "Monaco"),
            ("font-family-italic", "Menlo", "Monaco"),
            ("font-family-bold-italic", "Menlo", "Monaco"),
            ("font-feature", "calt", "-liga"),
            ("font-variation", "wght=400", "wght=500"),
            ("font-variation-bold", "wght=600", "wght=700"),
            ("font-variation-italic", "slnt=-5", "slnt=-10"),
            ("font-variation-bold-italic", "wght=600", "wght=700"),
            ("palette", "0=#000000", "1=#ffffff"),
            ("keybind", "cmd+t=tab.new", "cmd+w=close_surface"),
        ] {
            let original = format!("{key} = {first}\nconfig-file = child.conf\n");
            let first_save = apply_updates(&original, &[(key.into(), first.into())]);
            let second_save = apply_updates(&first_save, &[(key.into(), second.into())]);
            assert_eq!(
                second_save,
                format!("{key} = {second}\nconfig-file = child.conf\n"),
                "{key}"
            );
        }
    }

    // B05 (2026-09 audit): the settings panel edits the *primary* family, so
    // a non-empty `font-family` update replaces the head of the stack and
    // keeps every fallback after it.
    #[test]
    fn font_family_update_replaces_the_primary_slot_and_keeps_fallbacks() {
        let original = "font-family = A\nfont-family = B\n";

        let output = apply_updates(original, &[("font-family".to_string(), "C".to_string())]);

        assert_eq!(output, "font-family = C\nfont-family = B\n");
    }

    #[test]
    fn font_family_primary_slot_starts_after_the_last_reset_line() {
        let original = "font-family = A\nfont-family = \nfont-family = B\n";

        let output = apply_updates(original, &[("font-family".to_string(), "C".to_string())]);

        assert_eq!(output, "font-family = A\nfont-family = \nfont-family = C\n");
    }

    #[test]
    fn font_family_update_after_a_trailing_reset_is_appended() {
        let original = "font-family = A\nfont-family = \n";

        let output = apply_updates(original, &[("font-family".to_string(), "C".to_string())]);

        assert_eq!(output, "font-family = A\nfont-family = \nfont-family = C\n");
    }

    // B06: an empty `font-family` update is the list reset and must land
    // *after* every existing entry so the parser clears them all.
    #[test]
    fn empty_font_family_update_replaces_the_last_occurrence_and_resets_the_stack() {
        let original = "font-family = A\nfont-family = B\n";

        let output = apply_updates(original, &[("font-family".to_string(), String::new())]);

        assert_eq!(output, "font-family = A\nfont-family = \n");
        let (overrides, diagnostics) =
            crate::parse_overrides(Path::new("/tmp/noa-writer-test"), &output);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(overrides.font.families.is_empty());
    }

    #[test]
    fn font_family_round_trip_change_reset_change() {
        let original = "font-family = A\nfont-family = B\n";
        let changed = apply_updates(original, &[("font-family".to_string(), "C".to_string())]);
        let reset = apply_updates(&changed, &[("font-family".to_string(), String::new())]);
        let changed_again = apply_updates(&reset, &[("font-family".to_string(), "D".to_string())]);

        assert_eq!(
            changed_again,
            "font-family = C\nfont-family = \nfont-family = D\n"
        );
        let (overrides, _) =
            crate::parse_overrides(Path::new("/tmp/noa-writer-test"), &changed_again);
        assert_eq!(overrides.font.families, ["D"]);
    }

    #[test]
    fn consecutive_font_saves_before_include_use_the_latest_family() {
        let dir = unique_temp_dir("font-include");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config");
        fs::write(dir.join("child.conf"), "font-size = 18\n").unwrap();
        fs::write(&path, "font-family = Courier\nconfig-file = child.conf\n").unwrap();

        for family in ["Menlo", "Monaco"] {
            write_config_updates(&path, &[("font-family".into(), family.into())]).unwrap();
        }

        let source = fs::read_to_string(&path).unwrap();
        let (overrides, diagnostics) = crate::parse_overrides(&path, &source);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(overrides.font.families, ["Monaco"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    // N04: a family that only an include provides must not stay primary
    // after the panel saves a different one — with the include either
    // standing in for a missing slot or preceding the primary file's own.
    #[test]
    fn font_saves_beat_families_contributed_by_includes() {
        for (label, main) in [
            ("include-only", "config-file = fonts.conf\n"),
            (
                "include-before-slot",
                "config-file = fonts.conf\nfont-family = Menlo\n",
            ),
        ] {
            let dir = unique_temp_dir(&format!("font-include-{label}"));
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join("config");
            fs::write(
                dir.join("fonts.conf"),
                "font-family = Monaco\nfont-family = Courier\n",
            )
            .unwrap();
            fs::write(&path, main).unwrap();

            for family in ["Fira Code", "Hack"] {
                write_config_updates(&path, &[("font-family".into(), family.into())]).unwrap();
                let source = fs::read_to_string(&path).unwrap();
                let (overrides, diagnostics) = crate::parse_overrides(&path, &source);
                assert!(diagnostics.is_empty(), "{label}: {diagnostics:?}");
                assert_eq!(
                    overrides.font.families.first().map(String::as_str),
                    Some(family),
                    "{label}: the saved family heads the effective list"
                );
                assert!(
                    overrides.font.families.contains(&"Monaco".to_string())
                        && overrides.font.families.contains(&"Courier".to_string()),
                    "{label}: the include's families survive as fallbacks: {:?}",
                    overrides.font.families
                );
                assert_eq!(
                    overrides.font.families.len(),
                    3,
                    "{label}: no duplicate or stale entries: {:?}",
                    overrides.font.families
                );
            }
            // Untouched by the writer: the include file itself.
            assert_eq!(
                fs::read_to_string(dir.join("fonts.conf")).unwrap(),
                "font-family = Monaco\nfont-family = Courier\n"
            );
            fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn font_family_reset_clears_trailing_includes_after_reload() {
        let dir = unique_temp_dir("font-reset-include");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config");
        for key in [
            "font-family",
            "font-family-bold",
            "font-family-italic",
            "font-family-bold-italic",
        ] {
            fs::write(dir.join("child.conf"), format!("{key} = Monaco\n")).unwrap();
            fs::write(dir.join("last.conf"), format!("{key} = Courier\n")).unwrap();
            fs::write(
                &path,
                format!("{key} = Menlo\nconfig-file = child.conf\nconfig-file = last.conf\n"),
            )
            .unwrap();

            let updates = [(key.to_string(), String::new())];
            write_config_updates(&path, &updates).unwrap();
            let source = fs::read_to_string(&path).unwrap();
            let (overrides, diagnostics) = crate::parse_overrides(&path, &source);
            assert!(diagnostics.is_empty(), "{diagnostics:?}");
            assert_eq!(overrides.font, crate::FontConfig::default(), "{key}");
            assert_eq!(apply_updates(&source, &updates), source, "{key}");
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn key_after_include_is_replaced_in_place_only() {
        let original = "config-file = child.conf\nfont-size = 14\n";

        let output = apply_updates(original, &[("font-size".to_string(), "22".to_string())]);

        assert_eq!(output, "config-file = child.conf\nfont-size = 22\n");
    }

    #[test]
    fn saved_value_wins_over_trailing_include_after_reload() {
        let dir = unique_temp_dir("include-shadow");
        fs::create_dir_all(&dir).unwrap();
        let main_path = dir.join("config");
        fs::write(dir.join("child.conf"), "font-size = 18\n").unwrap();
        fs::write(&main_path, "font-size = 14\nconfig-file = child.conf\n").unwrap();

        write_config_updates(&main_path, &[("font-size".to_string(), "22".to_string())]).unwrap();

        let source = fs::read_to_string(&main_path).unwrap();
        let (overrides, diagnostics) = crate::parse_overrides(&main_path, &source);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(overrides.font_size, Some(22.0));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn absent_key_is_appended_at_end() {
        let original = "window-width = 100\n";

        let output = apply_updates(original, &[("theme".to_string(), "3024 Day".to_string())]);

        assert_eq!(output, "window-width = 100\ntheme = 3024 Day\n");
    }

    #[test]
    fn empty_original_appends_all_updates() {
        let output = apply_updates(
            "",
            &[
                ("font-size".to_string(), "18".to_string()),
                ("theme".to_string(), "3024 Day".to_string()),
            ],
        );

        assert_eq!(output, "font-size = 18\ntheme = 3024 Day\n");
    }

    #[test]
    fn empty_updates_leave_output_identical_to_input() {
        let original = "# a comment\nfont-size = 14\n";

        assert_eq!(apply_updates(original, &[]), original);
    }

    #[test]
    fn missing_trailing_newline_is_preserved_when_only_replacing() {
        let original = "font-size = 12";

        let output = apply_updates(original, &[("font-size".to_string(), "16".to_string())]);

        assert_eq!(output, "font-size = 16");
    }

    #[test]
    fn missing_trailing_newline_gains_one_on_append() {
        let original = "font-size = 12";

        let output = apply_updates(original, &[("theme".to_string(), "3024 Day".to_string())]);

        assert_eq!(output, "font-size = 12\ntheme = 3024 Day\n");
    }

    #[test]
    fn write_config_updates_creates_missing_file() {
        let dir = unique_temp_dir("missing-file");
        let config_path = dir.join("noa").join("config");

        write_config_updates(&config_path, &[("font-size".to_string(), "16".to_string())])
            .expect("write should succeed against a nonexistent file");

        let contents = fs::read_to_string(&config_path).unwrap();
        assert_eq!(contents, "font-size = 16\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_config_updates_is_atomic_via_rename() {
        let dir = unique_temp_dir("atomic");
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config");
        fs::write(&config_path, "font-size = 12\n").unwrap();

        write_config_updates(&config_path, &[("font-size".to_string(), "16".to_string())]).unwrap();

        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "font-size = 16\n"
        );
        // No leftover temp file after a successful rename.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_config_updates_preserves_existing_mode() {
        use std::os::unix::fs::PermissionsExt;

        for mode in [0o600u32, 0o640, 0o644] {
            let dir = unique_temp_dir(&format!("mode{mode:o}"));
            fs::create_dir_all(&dir).unwrap();
            let config_path = dir.join("config");
            fs::write(&config_path, "font-size = 12\n").unwrap();
            fs::set_permissions(&config_path, fs::Permissions::from_mode(mode)).unwrap();

            write_config_updates(&config_path, &[("font-size".to_string(), "16".to_string())])
                .unwrap();

            let got = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(got, mode, "mode {mode:o} not preserved");
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_config_updates_creates_new_file_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_temp_dir("newmode");
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config");

        write_config_updates(&config_path, &[("font-size".to_string(), "16".to_string())]).unwrap();

        let got = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(got, 0o600);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_config_updates_writes_through_symlink() {
        use std::os::unix::fs::symlink;

        let dir = unique_temp_dir("symlink");
        fs::create_dir_all(&dir).unwrap();
        let real_config = dir.join("real-config");
        let symlink_path = dir.join("config");
        fs::write(&real_config, "font-size = 12\n").unwrap();
        symlink(&real_config, &symlink_path).unwrap();

        write_config_updates(
            &symlink_path,
            &[("font-size".to_string(), "16".to_string())],
        )
        .unwrap();

        assert!(
            symlink_path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(&real_config).unwrap(),
            "font-size = 16\n"
        );
        assert_eq!(
            fs::read_to_string(&symlink_path).unwrap(),
            "font-size = 16\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    // B08 (2026-09 audit): a dotfiles-style symlink whose target does not
    // exist yet must be written *through* (creating the target and its
    // directory), never replaced by a regular file.
    #[cfg(unix)]
    #[test]
    fn write_config_updates_writes_through_a_dangling_symlink() {
        use std::os::unix::fs::symlink;

        let dir = unique_temp_dir("dangling-symlink");
        fs::create_dir_all(&dir).unwrap();
        let real_config = dir.join("dotfiles").join("noa").join("config");
        let symlink_path = dir.join("config");
        symlink(&real_config, &symlink_path).unwrap();
        assert!(!symlink_path.exists(), "precondition: link is dangling");

        write_config_updates(
            &symlink_path,
            &[("font-size".to_string(), "16".to_string())],
        )
        .unwrap();

        assert!(
            symlink_path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself must survive"
        );
        assert_eq!(
            fs::read_to_string(&real_config).unwrap(),
            "font-size = 16\n"
        );
        assert_eq!(
            fs::read_to_string(&symlink_path).unwrap(),
            "font-size = 16\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_config_updates_follows_relative_symlink_chains() {
        use std::os::unix::fs::symlink;

        let dir = unique_temp_dir("relative-symlink-chain");
        fs::create_dir_all(dir.join("store")).unwrap();
        // config -> store/link (relative) -> real (relative to store/)
        symlink("real", dir.join("store").join("link")).unwrap();
        symlink("store/link", dir.join("config")).unwrap();

        write_config_updates(
            &dir.join("config"),
            &[("font-size".to_string(), "16".to_string())],
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(dir.join("store").join("real")).unwrap(),
            "font-size = 16\n"
        );
        assert!(
            dir.join("config")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            dir.join("store")
                .join("link")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_config_updates_rejects_a_symlink_cycle() {
        use std::os::unix::fs::symlink;

        let dir = unique_temp_dir("symlink-cycle");
        fs::create_dir_all(&dir).unwrap();
        symlink(dir.join("b"), dir.join("a")).unwrap();
        symlink(dir.join("a"), dir.join("b")).unwrap();

        let err = write_config_updates(
            &dir.join("a"),
            &[("font-size".to_string(), "16".to_string())],
        )
        .unwrap_err();

        // Either the initial read (ELOOP from the OS) or `resolve_symlinks`'
        // hop cap reports it; what matters is that the link is untouched.
        assert!(
            dir.join("a")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            dir.join("b")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = err;
        fs::remove_dir_all(dir).unwrap();
    }
}
