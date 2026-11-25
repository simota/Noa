//! The code source: `TODO:` / `FIXME:` / `#TODO(agent):` markers in the
//! repository's tracked files, read with `git grep`, never written. A
//! marker is a task while it is in the code: it is known by its path and
//! its text, so a task keeps its runs while its line moves, and leaves the
//! list once the marker is removed. Nothing here closes one.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

use crate::model::types::{Millis, State, Task, TaskRef};
use crate::store::{fnv1a, without_git_env};

/// The markers taken, each followed by `:` or a `(…)` then `:`.
pub const MARKERS: [&str; 2] = ["TODO", "FIXME"];
/// The most markers listed: past it a repository's markers are noise.
pub const MAX_TASKS: usize = 500;
/// Directories of code the repository carries but does not own (at any
/// depth): their markers are another project's.
pub const VENDORED: [&str; 4] = ["vendor", "third_party", "third-party", "node_modules"];
/// A title's length at most, in characters.
const TITLE_CHARS: usize = 120;

/// The marker in `line`, if any: the whole text after it, trimmed (the
/// marker word itself when nothing follows). A marker counts only as a word of its
/// own (not `TODOS`, `xTODO`) and not inside a code span (prose about
/// markers, as in docs, quotes them so).
pub fn marker(line: &str) -> Option<String> {
    // Every place a marker word starts, in the order they stand on the
    // line: the first marker there wins, whichever word it is.
    let mut found: Vec<(usize, &str)> = MARKERS
        .iter()
        .flat_map(|word| line.match_indices(word).map(|(at, _)| (at, *word)))
        .collect();
    found.sort();
    for (at, word) in found {
        let before = line[..at].chars().next_back();
        let before = match before {
            Some('#') => line[..at - 1].chars().next_back(),
            other => other,
        };
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '`') {
            continue;
        }
        // Inside a code span (an odd number of backticks before it):
        // an example in prose, not a marker.
        if line[..at].matches('`').count() % 2 == 1 {
            continue;
        }
        let mut rest = &line[at + word.len()..];
        if let Some(tail) = rest.strip_prefix('(') {
            let Some(close) = tail.find(')') else {
                continue;
            };
            rest = &tail[close + 1..];
        }
        let Some(text) = rest.strip_prefix(':') else {
            continue;
        };
        let text = text.trim();
        let text = text.strip_suffix("*/").unwrap_or(text).trim();
        return Some(if text.is_empty() { word } else { text }.to_owned());
    }
    None
}

/// The tasks of `git grep -z -n -B1` output (`path\0line\0text\n` for
/// each marker's line and the line before it, `--` between runs of
/// lines), in the order listed, at most [`MAX_TASKS`]; `mtime` gives each
/// file's modification time.
///
/// A marker is known by its file and a digest of its whole line (the code
/// before the marker too) with the line just above it: not by its line
/// number, which moves as the file is edited, nor by its order among alike
/// markers, which moves when one is removed. Editing the line itself or the
/// one above makes it another marker. Only markers alike in both get `-2`,
/// `-3` in order.
pub fn parse(out: &[u8], mtime: impl Fn(&str) -> Millis) -> Vec<Task> {
    let mut lines: Vec<(String, u32, String)> = Vec::new();
    for record in out.split(|b| *b == b'\n') {
        let mut fields = record.splitn(3, |b| *b == 0);
        let (Some(path), Some(line), Some(text)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        lines.push((
            String::from_utf8_lossy(path).into_owned(),
            String::from_utf8_lossy(line).parse().unwrap_or(0),
            String::from_utf8_lossy(text).into_owned(),
        ));
    }
    let mut tasks: Vec<Task> = Vec::new();
    for (at, (path, number, text)) in lines.iter().enumerate() {
        let Some(whole) = marker(text) else {
            continue;
        };
        let line = text.trim();
        let above = at
            .checked_sub(1)
            .map(|i| &lines[i])
            .filter(|(p, n, _)| p == path && n + 1 == *number)
            .map_or("", |(_, _, t)| t.trim());
        let digest = format!("{:016x}", fnv1a(format!("{above}\n{line}").as_bytes()));
        let same = tasks
            .iter()
            .filter(|t| matches!(&t.r#ref, TaskRef::Code { path: p, key } if p == path && key.starts_with(&digest)))
            .count();
        let key = match same {
            0 => digest,
            n => format!("{digest}-{}", n + 1),
        };
        tasks.push(Task {
            r#ref: TaskRef::Code {
                path: path.clone(),
                key,
            },
            title: whole.chars().take(TITLE_CHARS).collect(),
            body: Some(format!("{path}:{number}\n\n{line}\n")),
            state: State::Open,
            labels: Vec::new(),
            updated: mtime(path),
            url: None,
            closed: None,
            problem: None,
        });
        if tasks.len() == MAX_TASKS {
            break;
        }
    }
    tasks
}

/// The markers in `repo`'s tracked text files.
pub fn scan(repo: &Path) -> Result<Vec<Task>, String> {
    let pattern = format!("(^|[^A-Za-z0-9_`])#?({})(\\([^)]*\\))?:", MARKERS.join("|"));
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .args(["grep", "-z", "-n", "-B1", "-I", "--no-color", "-E", "-e"])
        .arg(&pattern)
        .arg("--")
        .arg(".")
        .args(
            VENDORED
                .iter()
                .map(|dir| format!(":(exclude,glob)**/{dir}/**")),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    without_git_env(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("git を起動できません: {e}"))?;
    // 1: nothing matched.
    match out.status.code() {
        Some(0) => {}
        Some(1) if out.stderr.is_empty() => return Ok(Vec::new()),
        _ => {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
        }
    }
    let mtime = |path: &str| {
        std::fs::metadata(repo.join(path))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as Millis)
    };
    Ok(parse(&out.stdout, mtime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;

    #[test]
    fn a_marker_is_a_word_of_its_own_followed_by_a_colon() {
        assert_eq!(marker("// TODO: fix it").as_deref(), Some("fix it"));
        assert_eq!(marker("# FIXME(simota): later").as_deref(), Some("later"));
        assert_eq!(
            marker("    // #TODO(agent): DEFERRED — x").as_deref(),
            Some("DEFERRED — x")
        );
        assert_eq!(marker("/* TODO: wrap */").as_deref(), Some("wrap"));
        assert_eq!(marker("TODO:").as_deref(), Some("TODO"));
        // Two on a line: the first one there, whatever its word.
        assert_eq!(
            marker("// FIXME: race here // TODO: test").as_deref(),
            Some("race here // TODO: test")
        );
        // After a closed code span it counts again.
        assert_eq!(marker("`x` // TODO: real").as_deref(), Some("real"));
        for none in [
            "TODOS: no",
            "xTODO: no",
            "a TODO list",
            "TODO without colon",
            "prose about `#TODO(agent):` markers",
            "`TODO:` in backticks",
            "TODO(unclosed: no",
            "例: `// TODO: example` in prose",
            "two spans `a` and `// FIXME: b`",
        ] {
            assert_eq!(marker(none), None, "{none}");
        }
    }

    #[test]
    fn parse_reads_path_line_text_and_keys_by_text_not_line() {
        let out = b"src/a.rs\x0012\x00// TODO: one\nsrc/a.rs\x0040\x00// TODO: one\nweird:name.md\x003\x00FIXME: two\nsrc/b.rs\x001\x00no marker\n";
        let tasks = parse(out, |_| 7);
        assert_eq!(tasks.len(), 3);
        let (TaskRef::Code { path: p0, key: k0 }, TaskRef::Code { key: k1, .. }) =
            (&tasks[0].r#ref, &tasks[1].r#ref)
        else {
            panic!("{tasks:?}")
        };
        assert_eq!(p0, "src/a.rs");
        assert_eq!(format!("{k0}-2"), *k1);
        assert_eq!(tasks[0].title, "one");
        assert_eq!(tasks[0].updated, 7);
        assert_eq!(
            tasks[0].body.as_deref(),
            Some("src/a.rs:12\n\n// TODO: one\n")
        );
        assert!(matches!(&tasks[2].r#ref, TaskRef::Code { path, .. } if path == "weird:name.md"));
        // The same text on another line is the same task.
        let moved = parse(b"src/a.rs\x0099\x00// TODO: one\n", |_| 7);
        assert_eq!(moved[0].r#ref, tasks[0].r#ref);
    }

    /// Two markers alike in their first 120 characters are two tasks
    /// whose keys do not depend on each other: removing one leaves the
    /// other's key (and so its runs) as it was.
    #[test]
    fn a_key_comes_from_the_whole_text_not_the_shortened_title() {
        let head = "x".repeat(130);
        // As `git grep -B1` lists them: the second after the line above it.
        let both = format!(
            "a.rs\x001\x00// TODO: {head} first\na.rs\x002\x00fn a() {{}}\na.rs\x003\x00// TODO: {head} second\n"
        );
        let tasks = parse(both.as_bytes(), |_| 1);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].title, tasks[1].title, "shortened alike");
        assert_ne!(tasks[0].r#ref, tasks[1].r#ref);
        // The first removed: the second moves up a line, its line above stays.
        let second = format!("a.rs\x001\x00fn a() {{}}\na.rs\x002\x00// TODO: {head} second\n");
        assert_eq!(parse(second.as_bytes(), |_| 1)[0].r#ref, tasks[1].r#ref);
    }

    /// Two markers with the very same text in one file are two tasks, and
    /// removing the first does not hand its key (and its runs) to the
    /// second.
    #[test]
    fn removing_one_of_two_identical_markers_leaves_the_others_key() {
        let both = b"a.rs\x003\x00fn one() {} // TODO: x\na.rs\x009\x00fn two() {} // TODO: x\n";
        let tasks = parse(both, |_| 1);
        assert_eq!(tasks.len(), 2);
        assert_ne!(tasks[0].r#ref, tasks[1].r#ref);
        let second = b"a.rs\x008\x00fn two() {} // TODO: x\n";
        assert_eq!(parse(second, |_| 1)[0].r#ref, tasks[1].r#ref);
    }

    /// Markers on lines identical in full are told apart by the line
    /// above them, so removing the first keeps the second's key too.
    #[test]
    fn identical_marker_lines_are_told_apart_by_the_line_above() {
        // `git grep -z -n -B1`: each marker line after the line before it.
        let both = b"a.rs\x001\x00fn one() {\na.rs\x002\x00    // TODO: x\n--\na.rs\x005\x00fn two() {\na.rs\x006\x00    // TODO: x\n";
        let tasks = parse(both, |_| 1);
        assert_eq!(tasks.len(), 2);
        assert_ne!(tasks[0].r#ref, tasks[1].r#ref);
        assert!(!matches!(&tasks[1].r#ref, TaskRef::Code { key, .. } if key.contains('-')));
        assert_eq!(tasks[1].body.as_deref(), Some("a.rs:6\n\n// TODO: x\n"));
        // The first removed: the second, now on line 5, keeps its key.
        let second = parse(
            b"a.rs\x004\x00fn two() {\na.rs\x005\x00    // TODO: x\n",
            |_| 1,
        );
        assert_eq!(second[0].r#ref, tasks[1].r#ref);
        // A marker on the first line has nothing above it.
        let top = parse(b"a.rs\x001\x00// TODO: x\n", |_| 1);
        assert_eq!(top.len(), 1);
    }

    /// The reviewers' case through real `git grep`: two identical marker
    /// lines; the first removed (as its run's brief says); the second keeps
    /// its key, so the first one's runs are not handed to it.
    #[test]
    fn scan_keeps_the_remaining_markers_key_when_its_twin_is_removed() {
        let tmp = TempDir::new("code-twins");
        let repo = tmp.path();
        let git = |args: &[&str]| {
            let mut cmd = Command::new("git");
            cmd.arg("-C").arg(repo).args(args);
            without_git_env(&mut cmd);
            assert!(cmd.output().unwrap().status.success(), "{args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(
            repo.join("a.rs"),
            "fn one() {\n    // TODO: x\n}\n\nfn two() {\n    // TODO: x\n}\n",
        )
        .unwrap();
        git(&["add", "a.rs"]);
        let before = scan(repo).unwrap();
        assert_eq!(before.len(), 2);
        assert_ne!(before[0].r#ref, before[1].r#ref);
        std::fs::write(
            repo.join("a.rs"),
            "fn one() {\n}\n\nfn two() {\n    // TODO: x\n}\n",
        )
        .unwrap();
        let after = scan(repo).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].r#ref, before[1].r#ref);
        assert_eq!(after[0].body.as_deref(), Some("a.rs:5\n\n// TODO: x\n"));
    }

    #[test]
    fn scan_reads_tracked_files_only() {
        let tmp = TempDir::new("code-scan");
        let repo = tmp.path();
        let git = |args: &[&str]| {
            let mut cmd = Command::new("git");
            cmd.arg("-C").arg(repo).args(args);
            without_git_env(&mut cmd);
            assert!(cmd.output().unwrap().status.success(), "{args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("tracked.rs"), "fn a() {} // TODO: tracked\n").unwrap();
        std::fs::write(repo.join("untracked.rs"), "// TODO: untracked\n").unwrap();
        for dir in ["vendor/winit/src", "app/third_party", "web/node_modules/x"] {
            std::fs::create_dir_all(repo.join(dir)).unwrap();
            std::fs::write(repo.join(dir).join("lib.rs"), "// TODO: theirs\n").unwrap();
        }
        git(&["add", "-f", "."]);
        git(&["rm", "-q", "--cached", "untracked.rs"]);
        let tasks = scan(repo).unwrap();
        assert_eq!(tasks.len(), 1, "{tasks:?}");
        assert_eq!(tasks[0].title, "tracked");
        assert!(tasks[0].updated > 0);
        // Nothing matched is no task, not an error.
        std::fs::write(repo.join("tracked.rs"), "fn a() {}\n").unwrap();
        assert!(scan(repo).unwrap().is_empty());
        // Not a repository: an error.
        let other = TempDir::new("code-not-repo");
        assert!(scan(other.path()).is_err());
    }
}
