//! `config-file` recursive includes: splices an included file's directives
//! in at the point of the `config-file = <path>` directive (Ghostty
//! precedence — an included file's keys apply exactly where the include
//! appears, not before or after the whole document), with relative-path
//! resolution against the including file's directory, a `?`-prefixed
//! optional form, and cycle/depth guards.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::diagnostics::config_file_diagnostic;
use super::directives::parse_directives;
use super::{Diagnostic, Directive};

/// Include depth cap. A cycle is caught earlier by the visited-path set;
/// this is a backstop against pathologically deep (but acyclic) chains.
const MAX_INCLUDE_DEPTH: usize = 10;

/// Total-includes cap. The `visited` set only blocks a file from including one
/// of its own ancestors, so a DAG-shaped fan-out (each file including many
/// distinct files) re-expands work exponentially — the config-include analogue
/// of the XML "billion laughs" attack. This monotonic counter bounds the total
/// number of `config-file` expansions across the whole recursion, independent of
/// depth or path, so a hostile/mis-synced config cannot hang the terminal.
const MAX_INCLUDED_FILES: usize = 256;

/// A directive tagged with the path of the file it was declared in, so
/// per-directive diagnostics point at the file that actually owns the
/// value rather than the top-level config path.
pub(super) struct SourcedDirective {
    pub(super) path: PathBuf,
    pub(super) directive: Directive,
}

/// Parse `source` (the contents of `path`) and recursively expand every
/// `config-file` directive into the included file's own directives at that
/// point.
pub(super) fn expand_directives(
    path: &Path,
    source: &str,
) -> (Vec<SourcedDirective>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let mut visited = HashSet::new();
    visited.insert(canonical_or_self(path));
    let mut included_count = 0usize;
    let directives = expand(
        path,
        source,
        0,
        &mut visited,
        &mut included_count,
        &mut diagnostics,
    );
    (directives, diagnostics)
}

fn expand(
    path: &Path,
    source: &str,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    included_count: &mut usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<SourcedDirective> {
    let mut out = Vec::new();
    for directive in parse_directives(source) {
        if directive.key != "config-file" {
            out.push(SourcedDirective {
                path: path.to_path_buf(),
                directive,
            });
            continue;
        }

        let Some(raw) = directive.value.as_deref() else {
            diagnostics.push(config_file_diagnostic(path, "", "requires a path"));
            continue;
        };
        let (optional, raw) = match raw.strip_prefix('?') {
            Some(rest) => (true, rest),
            None => (false, raw),
        };
        if raw.is_empty() {
            diagnostics.push(config_file_diagnostic(path, raw, "requires a path"));
            continue;
        }

        if depth + 1 >= MAX_INCLUDE_DEPTH {
            diagnostics.push(config_file_diagnostic(
                path,
                raw,
                "exceeds the maximum include depth",
            ));
            continue;
        }

        let resolved = resolve_include_path(path, raw);
        let canonical = canonical_or_self(&resolved);
        if visited.contains(&canonical) {
            diagnostics.push(config_file_diagnostic(
                path,
                raw,
                "would create an include cycle",
            ));
            continue;
        }

        if *included_count >= MAX_INCLUDED_FILES {
            diagnostics.push(config_file_diagnostic(
                path,
                raw,
                "exceeds the maximum number of included files",
            ));
            continue;
        }
        *included_count += 1;

        let included_source = match fs::read_to_string(&resolved) {
            Ok(text) => text,
            Err(_) if optional => continue,
            Err(_) => {
                diagnostics.push(config_file_diagnostic(path, raw, "could not be read"));
                continue;
            }
        };

        visited.insert(canonical.clone());
        out.extend(expand(
            &resolved,
            &included_source,
            depth + 1,
            visited,
            included_count,
            diagnostics,
        ));
        visited.remove(&canonical);
    }
    out
}

/// Resolve an include path: absolute paths pass through, `~/...` expands
/// against the home directory, everything else resolves relative to the
/// *including* file's directory (not the process cwd), matching Ghostty.
fn resolve_include_path(including_path: &Path, raw: &str) -> PathBuf {
    let raw_path = Path::new(raw);
    if raw_path.is_absolute() {
        return raw_path.to_path_buf();
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    let base = including_path.parent().unwrap_or_else(|| Path::new("."));
    base.join(raw_path)
}

/// Canonicalize for cycle detection when possible; a path that doesn't
/// exist yet (or can't be canonicalized) still needs a stable key, so fall
/// back to the path as given.
fn canonical_or_self(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
