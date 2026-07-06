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

use crate::parser::parse_directives;

/// Applies `updates` (key, value pairs) to `original`, returning the updated
/// config text.
///
/// - A key already present is rewritten in place as `key = value`; if the
///   key occurs on multiple lines (duplicate directives), only the **last**
///   occurrence is replaced (Ghostty's last-wins resolution semantics) —
///   earlier occurrences are left untouched.
/// - A key absent from `original` is appended as a new `key = value` line at
///   the end.
/// - Every other line (comments, unknown keys, blank lines, other keys, and
///   the original line order) is preserved byte-for-byte.
pub fn apply_updates(original: &str, updates: &[(String, String)]) -> String {
    if updates.is_empty() {
        return original.to_string();
    }

    let directives = parse_directives(original);
    let mut replacements: HashMap<usize, String> = HashMap::new();
    let mut appended: Vec<&(String, String)> = Vec::new();

    for update @ (key, value) in updates {
        match directives
            .iter()
            .rev()
            .find(|directive| &directive.key == key)
        {
            Some(directive) => {
                replacements.insert(directive.line, format!("{key} = {value}"));
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
/// preserved rather than being replaced by a regular file.
pub fn write_config_updates(path: &Path, updates: &[(String, String)]) -> io::Result<()> {
    let original = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };

    let updated = apply_updates(&original, updates);

    let target = if path.exists() {
        fs::canonicalize(path)?
    } else {
        path.to_path_buf()
    };
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "config path has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;

    let tmp = target.with_extension("tmp");
    fs::write(&tmp, updated)?;
    fs::rename(&tmp, &target)?;

    Ok(())
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
        assert!(!config_path.with_extension("tmp").exists());
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
}
