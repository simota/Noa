//! Theme-settings-v2 favorites persistence (R-29/ADR-5): a newline-
//! delimited list of favorited theme names at
//! [`noa_config::theme_favorites_path`], written with the same
//! temp-file-then-rename pattern `session.rs::save` uses so a crash
//! mid-write can never truncate an existing good file. Deliberately not
//! JSON (no `serde` in this repo — `session.rs` hand-writes its own JSON
//! for a much richer schema; a favorites file is just a `HashSet<String>`,
//! for which one name per line is the simplest lossless format).
//!
//! This module never touches `noa_config::write_config_updates` or the
//! config file itself (AC-40/AC-41) — favorites are a UI preference, not a
//! config directive.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Parse a favorites file's contents: one theme name per line, blank lines
/// ignored, no other syntax. Never fails — an unparseable line simply can't
/// occur in this format, so every non-empty line becomes a favorite.
pub(crate) fn parse(text: &str) -> HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Serialize a favorites set back to the same one-name-per-line format,
/// sorted so the on-disk file is stable/diffable across saves instead of
/// reshuffling with `HashSet`'s arbitrary iteration order.
pub(crate) fn serialize(favorites: &HashSet<String>) -> String {
    let mut names: Vec<&str> = favorites.iter().map(String::as_str).collect();
    names.sort_unstable();
    let mut out = String::new();
    for name in names {
        out.push_str(name);
        out.push('\n');
    }
    out
}

/// Best-effort load: a missing or unreadable file yields an empty set
/// rather than blocking the overlay from opening (spec's documented
/// edge case — "起動時読込失敗は空のお気に入り集合にフォールバック").
pub(crate) fn load(path: &Path) -> HashSet<String> {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => HashSet::new(),
    }
}

/// Atomically write `favorites` to `path` (temp file + rename), creating
/// the parent directory if needed — mirrors `session.rs::save`'s pattern.
pub(crate) fn save(path: &Path, favorites: &HashSet<String>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serialize(favorites))?;
    fs::rename(&tmp, path)
}

/// `App`'s handle on the favorites store: lazily loaded (best-effort, empty
/// on a missing/unreadable file — spec's documented edge case) on first
/// use, so a fresh install with no favorites file yet never blocks the
/// overlay from opening. `set`/`epoch` are handed to
/// [`crate::theme_settings::ThemeSettings::set_favorites`] on every session
/// open and every successful toggle — `epoch` is what lets
/// `view_fingerprint` (ADR-2) tell "the set's *contents* changed" apart
/// from "a fresh `Arc` wrapping the same contents".
pub(crate) struct ThemeFavorites {
    set: Arc<HashSet<String>>,
    epoch: u64,
    path: Option<PathBuf>,
    loaded: bool,
}

impl ThemeFavorites {
    pub(crate) fn new() -> Self {
        Self {
            set: Arc::new(HashSet::new()),
            epoch: 0,
            path: None,
            loaded: false,
        }
    }

    fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        self.path = noa_config::theme_favorites_path();
        if let Some(path) = &self.path {
            self.set = Arc::new(load(path));
        }
    }

    /// The current set + epoch, for seeding a freshly opened
    /// `ThemeSettingsInit` (loading lazily on first call).
    pub(crate) fn snapshot(&mut self) -> (Arc<HashSet<String>>, u64) {
        self.ensure_loaded();
        (Arc::clone(&self.set), self.epoch)
    }

    /// Toggle `name`'s favorited status and persist immediately (R-29: a
    /// toggle round-trips to disk right away, not deferred to Enter/commit
    /// — favorites never touch the commit path at all, AC-40). Returns the
    /// new snapshot to hand to `ThemeSettings::set_favorites` on success;
    /// `Err` carries the write failure for the caller to surface (FM-09 —
    /// never silent) without having mutated `self.set`/`epoch` (so a
    /// retried toggle starts from the same known-good in-memory state, not
    /// a state that's already drifted from what's on disk).
    pub(crate) fn toggle(&mut self, name: &str) -> Result<(Arc<HashSet<String>>, u64), io::Error> {
        self.ensure_loaded();
        let Some(path) = self.path.clone() else {
            return Err(io::Error::other(
                "no writable config directory for the favorites file",
            ));
        };
        let mut next = (*self.set).clone();
        if !next.remove(name) {
            next.insert(name.to_string());
        }
        save(&path, &next)?;
        self.set = Arc::new(next);
        self.epoch += 1;
        Ok((Arc::clone(&self.set), self.epoch))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ignores_blank_lines_and_trims_whitespace() {
        let set = parse("3024 Day\n\n  Dracula  \nGruvbox Dark\n");
        assert_eq!(set.len(), 3);
        assert!(set.contains("3024 Day"));
        assert!(set.contains("Dracula"));
        assert!(set.contains("Gruvbox Dark"));
    }

    #[test]
    fn serialize_round_trips_through_parse() {
        let mut set = HashSet::new();
        set.insert("3024 Day".to_string());
        set.insert("Dracula".to_string());
        let text = serialize(&set);
        assert_eq!(parse(&text), set);
    }

    #[test]
    fn serialize_output_is_sorted_for_stable_diffs() {
        let mut set = HashSet::new();
        set.insert("Zenburn".to_string());
        set.insert("Alabaster".to_string());
        assert_eq!(serialize(&set), "Alabaster\nZenburn\n");
    }

    #[test]
    fn load_missing_file_yields_an_empty_set() {
        let dir = std::env::temp_dir().join(format!(
            "noa-theme-favorites-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("theme-favorites");
        assert!(load(&path).is_empty());
    }

    // AC-41 (R-29): saving into a tempdir where the favorites file doesn't
    // exist yet creates it, and this module never calls
    // `noa_config::write_config_updates`/touches the config file at all —
    // trivially true here since this module doesn't import that function,
    // but the test also proves the *file it does touch* is the favorites
    // file, not `config`.
    #[test]
    fn save_creates_a_missing_file_and_leaves_config_untouched() {
        let dir = std::env::temp_dir().join(format!(
            "noa-theme-favorites-save-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let favorites_path = dir.join("theme-favorites");
        let config_path = dir.join("config");
        assert!(!favorites_path.exists());

        let mut set = HashSet::new();
        set.insert("3024 Day".to_string());
        save(&favorites_path, &set).expect("save should succeed");

        assert!(favorites_path.exists());
        assert!(!config_path.exists());
        assert_eq!(load(&favorites_path), set);

        std::fs::remove_dir_all(dir).unwrap();
    }
}
