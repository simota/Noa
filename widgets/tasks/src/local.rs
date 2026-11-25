//! Local tasks: one Markdown file per task, `<repo>/.noa/tasks/t-<millis>[-n].md`
//! (L1-2). A fixed header of `key: value` lines between `---` lines, then
//! the body:
//!
//! ```text
//! ---
//! title: Fix the resize flicker
//! state: open
//! labels: bug, render
//! ---
//! The body, Markdown, kept byte for byte.
//! ```
//!
//! `title` is required; `state` is `open` (the default) or `closed`;
//! `labels` is comma-separated. Unknown keys are kept, in order, on
//! rewrite. Agents may write such files too (`skills/noa-tasks/SKILL.md`
//! is their copy of this format); a file that does not parse is a card
//! with its error, never dropped.

use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::fsx;
use crate::model::types::{Millis, POLL_MS, State, Task, TaskRef};

/// Under the repository.
pub const TASKS_DIR: &str = ".noa/tasks";

/// The header's fence line.
const FENCE: &str = "---";
/// A task file larger than this is a card with that problem, not read.
const MAX_FILE_BYTES: u64 = 1 << 20;
/// The most a state change adds to a file: a whole `state: closed` line.
const STATE_ROOM: u64 = "state: closed\n".len() as u64;

/// `<repo>/.noa/tasks`.
pub fn tasks_dir(repo: &Path) -> PathBuf {
    repo.join(TASKS_DIR)
}

/// A parsed local task file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub title: String,
    pub state: State,
    pub labels: Vec<String>,
    /// Header keys this widget does not know, in file order.
    pub extra: Vec<(String, String)>,
    pub body: String,
}

/// One line of `text` with its end (`\n`, or none on the last line).
fn lines_with_ends(text: &str) -> impl Iterator<Item = &str> {
    text.split_inclusive('\n')
}

/// A line without its `\n` or `\r\n`.
fn bare(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// A header line without its end, and the byte offset it starts at.
type HeaderLine<'a> = (usize, &'a str);

/// The header lines and the byte offset the body starts at; `Err` when a
/// fence is missing.
fn split(text: &str) -> Result<(Vec<HeaderLine<'_>>, usize), String> {
    let mut at = 0;
    let mut lines = lines_with_ends(text);
    match lines.next() {
        Some(first) if bare(first) == FENCE => at += first.len(),
        _ => return Err("1 行目が --- ではありません".to_owned()),
    }
    let mut header = Vec::new();
    for line in lines {
        let start = at;
        at += line.len();
        if bare(line) == FENCE {
            return Ok((header, at));
        }
        header.push((start, bare(line)));
    }
    Err("ヘッダーを閉じる --- がありません".to_owned())
}

/// `labels: a, b` → `["a", "b"]`: split on commas, trimmed, empty ones dropped.
fn split_labels(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Reads a task file's text; `Err` says why it does not parse.
pub fn parse(text: &str) -> Result<LocalFile, String> {
    let (header, body_at) = split(text)?;
    let mut title = None;
    let mut state = State::Open;
    let mut labels = Vec::new();
    let mut extra = Vec::new();
    for (n, (_, line)) in header.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(format!("{} 行目が key: value ではありません", n + 2));
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "title" => title = Some(value.to_owned()),
            "state" => {
                state = match value {
                    "open" => State::Open,
                    "closed" => State::Closed,
                    other => {
                        return Err(format!(
                            "state は open か closed です（{other:?} とあります）"
                        ));
                    }
                }
            }
            "labels" => labels = split_labels(value),
            _ => extra.push((key.to_owned(), value.to_owned())),
        }
    }
    let title = title
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "title がありません".to_owned())?;
    Ok(LocalFile {
        title,
        state,
        labels,
        extra,
        body: text[body_at..].to_owned(),
    })
}

fn state_word(state: State) -> &'static str {
    match state {
        State::Open => "open",
        State::Closed => "closed",
    }
}

/// The file's text: `parse(render(f)) == f`; extra keys kept in order.
pub fn render(file: &LocalFile) -> String {
    let mut out = format!(
        "{FENCE}\ntitle: {}\nstate: {}\n",
        file.title,
        state_word(file.state)
    );
    if !file.labels.is_empty() {
        out.push_str(&format!("labels: {}\n", file.labels.join(", ")));
    }
    for (key, value) in &file.extra {
        out.push_str(&format!("{key}: {value}\n"));
    }
    out.push_str(FENCE);
    out.push('\n');
    out.push_str(&file.body);
    out
}

/// The task of file `id` (`t-…`): an `Err` becomes a card with its problem.
pub fn task(id: &str, parsed: Result<LocalFile, String>, updated: Millis) -> Task {
    let r#ref = TaskRef::Local(id.to_owned());
    match parsed {
        Ok(file) => Task {
            r#ref,
            title: file.title,
            body: Some(file.body),
            state: file.state,
            labels: file.labels,
            updated,
            url: None,
            // #TODO(agent): DEFERRED — a local file records no close time, so 完了's 7 days count from its mtime and editing a closed task shows it again; recording one needs a header key in the format (and skills/noa-tasks/SKILL.md).
            closed: None,
            problem: None,
        },
        Err(why) => Task {
            r#ref,
            title: id.to_owned(),
            body: None,
            state: State::Open,
            labels: Vec::new(),
            updated,
            url: None,
            closed: None,
            problem: Some(format!("読めません: {why}")),
        },
    }
}

/// Whether `name` is a task file's (`t-….md`, nothing hidden or nested).
fn task_id(name: &str) -> Option<&str> {
    let id = name.strip_suffix(".md")?;
    (id.len() > 2 && id.starts_with("t-") && !id.contains('/')).then_some(id)
}

/// Task `id`'s file; ids come from the scan, but an intent's is checked.
fn file_of(repo: &Path, id: &str) -> io::Result<PathBuf> {
    match task_id(&format!("{id}.md")) {
        Some(_) if !id.contains('\0') => Ok(tasks_dir(repo).join(format!("{id}.md"))),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{id:?} はローカルタスクの ID ではありません"),
        )),
    }
}

/// A free id in `dir`: `t-<now>`, then `t-<now>-2`, `-3` …
pub fn new_id(dir: &Path, now: Millis) -> String {
    (1u32..)
        .map(|n| match n {
            1 => format!("t-{now}"),
            n => format!("t-{now}-{n}"),
        })
        // A link at the name is taken too.
        .find(|id| fs::symlink_metadata(dir.join(format!("{id}.md"))).is_err())
        .expect("an unbounded range has a free id")
}

/// Writes `file` as a new task under `repo`; its id.
///
/// Published whole: the text goes into a hidden temporary file in the
/// tasks folder first (made exclusively, never through a link), which is
/// then hard-linked to the task's name (`linkat`, which fails on any file
/// or link already there) and unlinked. A reader never sees the name with
/// part of the text, and a crash leaves only the hidden file.
pub fn create(repo: &Path, file: &LocalFile, now: Millis) -> io::Result<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let text = readable(file)?;
    let dir = tasks_dir(repo);
    fsx::create_dirs(repo, &dir)?;
    let fd = fsx::open_dir(repo, &dir, false)?;
    // Not a `t-*.md` name: the scan and agents pass over it.
    let tmp = OsString::from(format!(
        ".new-{}-{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    fsx::write_new_at(&fd, &tmp, text.as_bytes())?;
    // Names are tried on the handle itself, never looked up by path: the
    // path may name another folder by now. A file or link at a name is
    // left as it is, and the next name is tried.
    let mut n = 1u32;
    let published = loop {
        let id = match n {
            1 => format!("t-{now}"),
            n => format!("t-{now}-{n}"),
        };
        match link_at(&fd, &tmp, OsStr::new(&format!("{id}.md"))) {
            Ok(()) => break Ok(id),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => break Err(e),
        }
    };
    let removed = unlink_at(&fd, &tmp);
    match (published, removed) {
        (Ok(id), Ok(())) => Ok(id),
        // The task is there; only the hidden file is left over.
        (Ok(id), Err(e)) => {
            eprintln!("tasks: {} を消せません: {e}", dir.join(&tmp).display());
            Ok(id)
        }
        (Err(e), _) => Err(e),
    }
}

/// `name` as a C string for the `*at` calls.
fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "名前に NUL が含まれます"))
}

/// A second name `to` for `from` in the directory `dir`; `AlreadyExists`
/// when `to` is taken by anything, a link included (never followed).
fn link_at(dir: &OwnedFd, from: &OsStr, to: &OsStr) -> io::Result<()> {
    let (from, to) = (c_name(from)?, c_name(to)?);
    let fd = dir.as_raw_fd();
    // SAFETY: `fd` is an open directory; both names are C strings. Flags 0:
    // `from` is not followed.
    if unsafe { libc::linkat(fd, from.as_ptr(), fd, to.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Removes `name` from the directory `dir`.
fn unlink_at(dir: &OwnedFd, name: &OsStr) -> io::Result<()> {
    let name = c_name(name)?;
    // SAFETY: `dir` is an open directory and `name` a C string.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Replaces task `id`'s file with `file` (`fsx::write_replace`: temp + rename).
pub fn write(repo: &Path, id: &str, file: &LocalFile) -> io::Result<()> {
    let path = file_of(repo, id)?;
    fsx::write_replace(repo, &path, readable(file)?.as_bytes())
}

/// `file`'s text, refused when [`read`] would refuse it back (over
/// [`MAX_FILE_BYTES`]): a task written must stay a task, not a card that
/// cannot be read, edited or run.
fn readable(file: &LocalFile) -> io::Result<String> {
    let text = render(file);
    // Room for the state line 完了にする / 再開 may add or lengthen, so a
    // task written can always change state and still be read back.
    if text.len() as u64 > MAX_FILE_BYTES - STATE_ROOM {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "タスクが大きすぎます（{} バイト、上限 {} バイト）",
                text.len(),
                MAX_FILE_BYTES - STATE_ROOM
            ),
        ));
    }
    Ok(text)
}

/// Refuses `text` that [`read`] would refuse back.
fn fits(text: &str) -> io::Result<()> {
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("状態を変えると {} KiB を超えます", MAX_FILE_BYTES >> 10),
        ));
    }
    Ok(())
}

/// Task `id`'s file text, read without following a link.
pub fn read(repo: &Path, id: &str) -> io::Result<String> {
    let path = file_of(repo, id)?;
    let (bytes, over) = fsx::read_capped(fsx::open_read(repo, &path)?, MAX_FILE_BYTES)?;
    if over {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} は {} KiB を超えています",
                path.display(),
                MAX_FILE_BYTES >> 10
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} は UTF-8 ではありません", path.display()),
        )
    })
}

/// Rewrites only task `id`'s `state`: its `state:` line, or one added after
/// the opening `---`; every other byte stays.
pub fn set_state(repo: &Path, id: &str, state: State) -> io::Result<()> {
    let text = read(repo, id)?;
    let invalid = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
    parse(&text).map_err(invalid)?;
    let (header, _) = split(&text).map_err(invalid)?;
    let line = format!("state: {}", state_word(state));
    let state_at = header.iter().rev().find(|(_, l)| {
        l.split_once(':')
            .is_some_and(|(key, _)| key.trim() == "state")
    });
    let new = match state_at {
        Some((start, old)) => {
            let end = start + old.len();
            format!("{}{line}{}", &text[..*start], &text[end..])
        }
        None => {
            let first = text.find('\n').map_or(text.len(), |i| i + 1);
            format!("{}{line}\n{}", &text[..first], &text[first..])
        }
    };
    fits(&new)?;
    fsx::write_replace(repo, &file_of(repo, id)?, new.as_bytes())
}

/// A file's `(len, mtime ns, (ctime, mode))`: what [`Scan`] compares. The
/// ctime and mode move when only its permissions change (chmod), which
/// can make it readable or not.
type Stamp = (u64, u128, (i64, i64, u32));

/// Watches `<repo>/.noa/tasks/` for changes.
pub struct Scan {
    repo: PathBuf,
    /// When it last looked.
    last: Option<Millis>,
    /// What it saw then, by id.
    seen: Option<BTreeMap<String, Stamp>>,
}

impl Scan {
    pub fn new(repo: PathBuf) -> Scan {
        Scan {
            repo,
            last: None,
            seen: None,
        }
    }

    /// Top-level t-*.md only; Some when a file's stamp (length, mtime,
    /// ctime, mode) or the set changed, at most every POLL_MS.
    pub fn poll(&mut self, now: Millis) -> Option<Vec<Task>> {
        if self
            .last
            .is_some_and(|last| now < last.saturating_add(POLL_MS))
        {
            return None;
        }
        self.last = Some(now);
        let stamps = self.stamps();
        if self.seen.as_ref() == Some(&stamps) {
            return None;
        }
        let tasks = stamps
            .iter()
            .map(|(id, (_, mtime, _))| {
                let updated = (mtime / 1_000_000) as Millis;
                let parsed = read(&self.repo, id)
                    .map_err(|e| e.to_string())
                    .and_then(|t| parse(&t));
                task(id, parsed, updated)
            })
            .collect();
        self.seen = Some(stamps);
        Some(tasks)
    }

    /// Every task file's stamp; a folder that is missing (or a link) has none.
    fn stamps(&self) -> BTreeMap<String, Stamp> {
        let dir = tasks_dir(&self.repo);
        let mut stamps = BTreeMap::new();
        if fsx::check_no_links(&self.repo, &dir).is_err() {
            return stamps;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            return stamps;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(task_id) else {
                continue;
            };
            // Not followed: a link is a card whose problem says so.
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            let changed = {
                use std::os::unix::fs::MetadataExt;
                (meta.ctime(), meta.ctime_nsec(), meta.mode())
            };
            stamps.insert(id.to_owned(), (meta.len(), mtime, changed));
        }
        stamps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;
    use std::os::unix::fs::symlink;
    use std::time::{Duration, SystemTime};

    fn file(title: &str) -> LocalFile {
        LocalFile {
            title: title.to_owned(),
            state: State::Open,
            labels: Vec::new(),
            extra: Vec::new(),
            body: String::new(),
        }
    }

    #[test]
    fn ac08_parse_reads_the_header_and_keeps_the_body_byte_for_byte() {
        let text = "---\ntitle:  Fix resize \nlabels: bug ,render,, ui\nowner: me\nstate: closed\npriority: high: really\n---\n# Body\n\n  indented\r\ntrailing  \n\n";
        let parsed = parse(text).unwrap();
        assert_eq!(parsed.title, "Fix resize");
        assert_eq!(parsed.state, State::Closed);
        assert_eq!(parsed.labels, ["bug", "render", "ui"]);
        assert_eq!(
            parsed.extra,
            [
                ("owner".to_owned(), "me".to_owned()),
                ("priority".to_owned(), "high: really".to_owned())
            ]
        );
        assert_eq!(parsed.body, "# Body\n\n  indented\r\ntrailing  \n\n");
        // state defaults to open; no body at all is an empty one.
        let bare = parse("---\ntitle: a\n---").unwrap();
        assert_eq!((bare.state, bare.body.as_str()), (State::Open, ""));
        assert!(bare.labels.is_empty() && bare.extra.is_empty());
        // CRLF files.
        assert_eq!(parse("---\r\ntitle: a\r\n---\r\nb").unwrap().body, "b");
    }

    #[test]
    fn ac08_parse_refuses_what_is_not_a_task_file() {
        for (text, why) in [
            ("title: a\n---\n", "---"),
            ("\n---\ntitle: a\n---\n", "---"),
            ("---\ntitle: a\nbody without a fence\n", "---"),
            ("---\nstate: open\n---\n", "title"),
            ("---\ntitle:   \n---\n", "title"),
            ("---\ntitle: a\nstate: done\n---\n", "state"),
            ("---\ntitle: a\nstate: Closed\n---\n", "state"),
            ("---\ntitle: a\njust words\n---\n", "key: value"),
        ] {
            let err = parse(text).unwrap_err();
            assert!(err.contains(why), "{text:?}: {err}");
        }
    }

    #[test]
    fn ac08_render_round_trips_and_keeps_extra_keys_in_order() {
        let files = [
            file("a"),
            LocalFile {
                title: "Reflow: keep the cursor row".into(),
                state: State::Closed,
                labels: vec!["bug".into(), "grid".into()],
                extra: vec![("zeta".into(), "1".into()), ("alpha".into(), "x: y".into())],
                body: "line\n\n---\nnot a fence for the header\n".into(),
            },
        ];
        for f in &files {
            let text = render(f);
            assert_eq!(&parse(&text).unwrap(), f, "{text}");
        }
        let text = render(&files[1]);
        assert!(text.find("zeta: 1").unwrap() < text.find("alpha: x: y").unwrap());
        assert!(text.starts_with("---\ntitle: Reflow: keep the cursor row\nstate: closed\n"));
    }

    #[test]
    fn ac09_the_skills_example_task_file_parses() {
        let skill = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/noa-tasks/SKILL.md");
        let text = fs::read_to_string(&skill).unwrap();
        let start = text.find("```text\n").expect("a text block") + "```text\n".len();
        let end = start + text[start..].find("```").unwrap();
        let parsed = parse(&text[start..end]).unwrap();
        assert_eq!(
            parsed.title,
            "Reflow loses the cursor row on a narrow resize"
        );
        assert_eq!(parsed.state, State::Open);
        assert_eq!(parsed.labels, ["bug", "grid"]);
        assert!(parsed.body.starts_with("Free Markdown body"));
    }

    /// The skill's create example (its `sh` block that writes `$tmp`), a
    /// temp repository, and a PATH whose `date` always says the same second.
    fn skill_create_rig(name: &str) -> (TempDir, PathBuf, String, String) {
        let skill = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/noa-tasks/SKILL.md");
        let text = fs::read_to_string(&skill).unwrap();
        let script = text
            .split("```sh\n")
            .skip(1)
            .map(|block| &block[..block.find("```").unwrap()])
            .find(|block| block.contains("cat > \"$tmp\""))
            .expect("the create example")
            .to_owned();
        let tmp = TempDir::new(name);
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let date = bin.join("date");
        fs::write(&date, "#!/bin/sh\necho 1700000000\n").unwrap();
        fs::set_permissions(&date, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let git = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(git.success());
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        (tmp, repo, script, path)
    }

    fn run_skill_create(repo: &Path, script: &str, path: &str) {
        let out = std::process::Command::new("sh")
            .args(["-c", script])
            .current_dir(repo)
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn task_names(repo: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(tasks_dir(repo))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The skill's shell example, run twice in the same second, must leave
    /// two tasks: a second `mv` onto the first one's name would lose it.
    #[test]
    fn ac09_the_skills_create_example_run_twice_in_one_second_keeps_both() {
        let (_tmp, repo, script, path) = skill_create_rig("skill-create");
        run_skill_create(&repo, &script, &path);
        run_skill_create(&repo, &script, &path);
        assert_eq!(
            task_names(&repo),
            ["t-1700000000000-2.md", "t-1700000000000.md"]
        );
    }

    /// A dangling link at the name is taken (as `create` treats it, AC-10):
    /// the example passes over it to `-2` instead of giving up and
    /// deleting what it wrote.
    #[test]
    fn ac09_the_skills_create_example_passes_over_a_dangling_link() {
        let (_tmp, repo, script, path) = skill_create_rig("skill-create-link");
        fs::create_dir_all(tasks_dir(&repo)).unwrap();
        std::os::unix::fs::symlink("nowhere.md", tasks_dir(&repo).join("t-1700000000000.md"))
            .unwrap();
        run_skill_create(&repo, &script, &path);
        assert_eq!(
            task_names(&repo),
            ["t-1700000000000-2.md", "t-1700000000000.md"]
        );
        assert!(
            fs::symlink_metadata(tasks_dir(&repo).join("t-1700000000000.md"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    /// A task the scan would refuse to read (over 1 MiB) is refused when
    /// written, by create and write alike, and nothing is left behind.
    #[test]
    fn a_task_too_large_to_read_back_is_not_written() {
        let tmp = TempDir::new("local-too-large");
        let repo = tmp.path();
        let mut big = file("big");
        big.body = "x".repeat(MAX_FILE_BYTES as usize);
        assert!(create(repo, &big, 5).is_err());
        assert!(
            !tasks_dir(repo).exists(),
            "a refused create made the folder"
        );
        let id = create(repo, &file("small"), 6).unwrap();
        assert!(write(repo, &id, &big).is_err());
        assert_eq!(
            read(repo, &id).map(|t| parse(&t).unwrap()).unwrap(),
            file("small")
        );
        // Exactly at the limit (what read takes, less room for a state
        // change) still round-trips.
        let mut edge = file("edge");
        let header = render(&edge).len();
        edge.body = "y".repeat((MAX_FILE_BYTES - STATE_ROOM) as usize - header);
        write(repo, &id, &edge).unwrap();
        assert_eq!(parse(&read(repo, &id).unwrap()).unwrap(), edge);
    }

    /// The largest task `write` takes can still be closed and reopened and
    /// read back: a state change grows the file (`state: closed`, or a
    /// state line added), and never past what `read` takes.
    #[test]
    fn the_largest_task_written_can_still_be_closed_and_read_back() {
        let tmp = TempDir::new("local-largest-state");
        let repo = tmp.path();
        let id = create(repo, &file("small"), 6).unwrap();
        let mut edge = file("edge");
        let mut n = MAX_FILE_BYTES as usize - render(&edge).len();
        edge.body = "y".repeat(n);
        while write(repo, &id, &edge).is_err() {
            n -= 1;
            edge.body.truncate(n);
        }
        set_state(repo, &id, State::Closed).unwrap();
        assert_eq!(
            parse(&read(repo, &id).unwrap()).unwrap().state,
            State::Closed
        );
        set_state(repo, &id, State::Open).unwrap();
        assert_eq!(parse(&read(repo, &id).unwrap()).unwrap().state, State::Open);
        // A file an agent wrote right at the limit, with no state line:
        // closing it is refused with why, and it stays readable.
        let path = tasks_dir(repo).join(format!("{id}.md"));
        let head = "---\ntitle: t\n---\n";
        fs::write(
            &path,
            format!("{head}{}", "z".repeat(MAX_FILE_BYTES as usize - head.len())),
        )
        .unwrap();
        assert!(set_state(repo, &id, State::Closed).is_err());
        assert!(read(repo, &id).is_ok());
    }

    #[test]
    fn ac10_create_names_t_millis_then_dash_two_and_never_writes_through_a_link() {
        let tmp = TempDir::new("local-create");
        let repo = tmp.path();
        assert_eq!(create(repo, &file("one"), 77).unwrap(), "t-77");
        assert_eq!(create(repo, &file("two"), 77).unwrap(), "t-77-2");
        assert_eq!(new_id(&tasks_dir(repo), 77), "t-77-3");
        let text = fs::read_to_string(tasks_dir(repo).join("t-77-2.md")).unwrap();
        assert_eq!(parse(&text).unwrap(), file("two"));
        // A link where the next name would go is passed over, not followed.
        let target = repo.join("outside.md");
        symlink(&target, tasks_dir(repo).join("t-77-3.md")).unwrap();
        assert_eq!(create(repo, &file("three"), 77).unwrap(), "t-77-4");
        assert!(!target.exists());
    }

    #[test]
    fn ac10_write_replaces_by_temp_and_rename_never_through_a_link() {
        let tmp = TempDir::new("local-write");
        let repo = tmp.path();
        let id = create(repo, &file("one"), 5).unwrap();
        let path = tasks_dir(repo).join(format!("{id}.md"));
        let before = fs::metadata(&path).unwrap();
        write(repo, &id, &file("edited")).unwrap();
        let after = fs::metadata(&path).unwrap();
        use std::os::unix::fs::MetadataExt as _;
        assert_ne!(before.ino(), after.ino(), "a new file renamed over it");
        assert_eq!(
            parse(&fs::read_to_string(&path).unwrap()).unwrap().title,
            "edited"
        );
        // No temporary file is left.
        let names: Vec<_> = fs::read_dir(tasks_dir(repo))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        // A link at the name is replaced, its target left alone.
        let target = repo.join("victim.md");
        fs::write(&target, "keep").unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&target, &path).unwrap();
        write(repo, &id, &file("again")).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
        assert!(
            !fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // An id that is not a task's is refused.
        assert!(write(repo, "../x", &file("x")).is_err());
        assert!(write(repo, "notes", &file("x")).is_err());
    }

    #[test]
    fn ac10_set_state_changes_only_the_state() {
        let tmp = TempDir::new("local-state");
        let repo = tmp.path();
        fs::create_dir_all(tasks_dir(repo)).unwrap();
        let path = tasks_dir(repo).join("t-1.md");
        let original =
            "---\ntitle: a\nlabels: x,y\nowner:me\nstate: open\n---\nbody\nstate: open\n";
        fs::write(&path, original).unwrap();
        set_state(repo, "t-1", State::Closed).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            original.replacen("state: open\n---", "state: closed\n---", 1)
        );
        set_state(repo, "t-1", State::Open).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        // No state line: one is added after the fence.
        fs::write(&path, "---\ntitle: a\n---\nb").unwrap();
        set_state(repo, "t-1", State::Closed).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "---\nstate: closed\ntitle: a\n---\nb"
        );
        // A file that does not parse is left as it is.
        fs::write(&path, "no header").unwrap();
        assert!(set_state(repo, "t-1", State::Closed).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "no header");
    }

    #[test]
    fn ac10_a_broken_file_is_a_card_and_other_files_are_ignored() {
        let tmp = TempDir::new("local-scan");
        let repo = tmp.path();
        let dir = tasks_dir(repo);
        fs::create_dir_all(dir.join("runs/run-1")).unwrap();
        fs::write(dir.join("runs/run-1/task.md"), "---\ntitle: brief\n---\n").unwrap();
        fs::write(dir.join("t-2.md"), "---\ntitle: good\nlabels: a\n---\nbody").unwrap();
        fs::write(dir.join("t-3.md"), "not a task").unwrap();
        fs::write(dir.join("notes.md"), "---\ntitle: notes\n---\n").unwrap();
        fs::write(dir.join(".t-4.md.tmp"), "---\ntitle: half\n---\n").unwrap();
        fs::write(dir.join("t-5.txt"), "---\ntitle: txt\n---\n").unwrap();
        fs::create_dir_all(dir.join("t-6.md")).unwrap();
        symlink(dir.join("t-2.md"), dir.join("t-7.md")).unwrap();
        let tasks = Scan::new(repo.to_owned()).poll(0).unwrap();
        let ids: Vec<&TaskRef> = tasks.iter().map(|t| &t.r#ref).collect();
        assert_eq!(
            ids,
            [
                &TaskRef::Local("t-2".into()),
                &TaskRef::Local("t-3".into()),
                &TaskRef::Local("t-7".into())
            ]
        );
        assert_eq!(tasks[0].title, "good");
        assert_eq!(tasks[0].body.as_deref(), Some("body"));
        assert_eq!(tasks[0].labels, ["a"]);
        assert!(tasks[0].problem.is_none());
        assert_eq!(tasks[1].title, "t-3");
        assert!(
            tasks[1].problem.as_deref().unwrap().contains("---"),
            "{:?}",
            tasks[1]
        );
        // A link is a card that says so, never followed.
        assert!(tasks[2].problem.is_some(), "{:?}", tasks[2]);
        // task(): Ok and Err.
        let t = task("t-9", Err("why".into()), 4);
        assert_eq!((t.updated, t.state, t.body), (4, State::Open, None));
        assert!(t.problem.unwrap().contains("why"));
    }

    /// A FIFO named like a task (no writer) must not block the scan, which
    /// runs on the feed's only worker: it is a card with its problem.
    #[test]
    fn ac10_a_fifo_named_like_a_task_is_a_card_and_does_not_block_the_scan() {
        let tmp = TempDir::new("local-fifo");
        let repo = tmp.path().to_path_buf();
        fs::create_dir_all(tasks_dir(&repo)).unwrap();
        let fifo = tasks_dir(&repo).join("t-1.md");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut scan = Scan::new(repo);
            let _ = tx.send(scan.poll(10_000));
        });
        let tasks = rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("the scan blocked on the FIFO")
            .expect("a listing");
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].problem.is_some(), "{:?}", tasks[0]);
        // Unblock the stuck reader, if any, so the test leaves no thread.
        let _ = std::os::unix::fs::OpenOptionsExt::custom_flags(
            fs::OpenOptions::new().write(true),
            libc::O_NONBLOCK,
        )
        .open(&fifo);
    }

    /// The tasks folder swapped for another while `create` runs (its
    /// directory handle on one, its path on the other) must not make it
    /// try the same taken name forever.
    #[test]
    fn create_ends_while_the_tasks_folder_is_swapped_under_it() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let tmp = TempDir::new("local-swap");
        let repo = tmp.path().to_path_buf();
        let dir = tasks_dir(&repo);
        let other = repo.join(".noa/other");
        let parked = repo.join(".noa/parked");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(dir.join("t-77.md"), "---\ntitle: a\n---\n").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let (stop, dir, other, parked) = (
                Arc::clone(&stop),
                dir.clone(),
                other.clone(),
                parked.clone(),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = fs::rename(&dir, &parked);
                    let _ = fs::rename(&other, &dir);
                    let _ = fs::rename(&parked, &other);
                }
            })
        };
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let repo = repo.clone();
            std::thread::spawn(move || {
                for _ in 0..3000 {
                    let _ = create(&repo, &file("x"), 77);
                }
                let _ = tx.send(());
            });
        }
        let ended = rx.recv_timeout(std::time::Duration::from_secs(20));
        stop.store(true, Ordering::Relaxed);
        swapper.join().unwrap();
        assert!(ended.is_ok(), "create looped on a taken name");
    }

    /// A file made readable again (or unreadable) is read again at the next
    /// look, though its length and mtime did not change.
    #[test]
    fn ac11_a_change_of_permissions_alone_is_seen() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("local-perms");
        let repo = tmp.path().to_path_buf();
        let id = create(&repo, &file("one"), 5).unwrap();
        let path = tasks_dir(&repo).join(format!("{id}.md"));
        let set = |mode: u32| fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        set(0o000);
        if fs::read(&path).is_ok() {
            set(0o644);
            return; // root reads anything: nothing to see here
        }
        let mut scan = Scan::new(repo.clone());
        let tasks = scan.poll(10_000).unwrap();
        assert!(tasks[0].problem.is_some(), "{tasks:?}");
        set(0o644);
        let tasks = scan.poll(10_000 + POLL_MS).expect("seen again");
        assert_eq!(tasks[0].problem, None, "{tasks:?}");
        set(0o000);
        let tasks = scan.poll(10_000 + 2 * POLL_MS).expect("seen again");
        assert!(tasks[0].problem.is_some(), "{tasks:?}");
        set(0o644);
    }

    /// The tasks folder swapped for a link to a folder outside the
    /// repository while a task is saved: nothing outside is written.
    #[test]
    fn a_save_never_writes_through_a_folder_swapped_for_a_link() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let tmp = TempDir::new("local-swap-link");
        let repo = tmp.path().to_path_buf();
        let id = create(&repo, &file("inside"), 5).unwrap();
        let dir = tasks_dir(&repo);
        let outside = repo.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let victim = outside.join(format!("{id}.md"));
        fs::write(&victim, "untouched\n").unwrap();
        let parked = repo.join(".noa/parked");
        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let (stop, dir, outside, parked) = (
                Arc::clone(&stop),
                dir.clone(),
                outside.clone(),
                parked.clone(),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = fs::rename(&dir, &parked);
                    let _ = std::os::unix::fs::symlink(&outside, &dir);
                    let _ = fs::remove_file(&dir);
                    let _ = fs::rename(&parked, &dir);
                }
            })
        };
        for _ in 0..3000 {
            let _ = write(&repo, &id, &file("written"));
            let _ = set_state(&repo, &id, State::Closed);
        }
        stop.store(true, Ordering::Relaxed);
        swapper.join().unwrap();
        let left: Vec<String> = fs::read_dir(&outside)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched\n");
        assert_eq!(left, [format!("{id}.md")], "files appeared outside");
    }

    /// A tasks folder that cannot be written: create and write fail with
    /// the error and leave nothing behind (no task, no temporary file).
    #[test]
    fn writes_into_a_folder_that_cannot_be_written_fail_and_leave_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("local-read-only");
        let repo = tmp.path().to_path_buf();
        let id = create(&repo, &file("one"), 5).unwrap();
        let dir = tasks_dir(&repo);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        let probe = dir.join(".probe");
        if fs::write(&probe, "").is_ok() {
            let _ = fs::remove_file(&probe);
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
            return; // root writes anywhere: nothing to see here
        }
        let before: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(create(&repo, &file("two"), 6).is_err());
        assert!(write(&repo, &id, &file("changed")).is_err());
        assert!(set_state(&repo, &id, State::Closed).is_err());
        let after: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(before, after);
        assert_eq!(parse(&read(&repo, &id).unwrap()).unwrap(), file("one"));
    }

    /// The tasks folder swapped for a link to a folder outside the
    /// repository while a task is read: what is outside is never read as
    /// the task.
    #[test]
    fn a_read_never_goes_through_a_folder_swapped_for_a_link() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let tmp = TempDir::new("local-read-swap");
        let repo = tmp.path().to_path_buf();
        let id = create(&repo, &file("inside"), 5).unwrap();
        let dir = tasks_dir(&repo);
        let outside = repo.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join(format!("{id}.md")), render(&file("OUTSIDE"))).unwrap();
        let parked = repo.join(".noa/parked");
        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let (stop, dir, outside, parked) = (
                Arc::clone(&stop),
                dir.clone(),
                outside.clone(),
                parked.clone(),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = fs::rename(&dir, &parked);
                    let _ = std::os::unix::fs::symlink(&outside, &dir);
                    let _ = fs::remove_file(&dir);
                    let _ = fs::rename(&parked, &dir);
                }
            })
        };
        let mut leaked = 0;
        for _ in 0..5000 {
            if let Ok(text) = read(&repo, &id)
                && text.contains("OUTSIDE")
            {
                leaked += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        swapper.join().unwrap();
        assert_eq!(leaked, 0, "read the file outside {leaked} times");
    }

    #[test]
    fn ac11_scan_polls_at_most_every_two_seconds_and_sees_adds_edits_and_removals() {
        let tmp = TempDir::new("local-poll");
        let repo = tmp.path();
        let dir = tasks_dir(repo);
        let mut scan = Scan::new(repo.to_owned());
        // The first look always answers, even with no folder.
        assert_eq!(scan.poll(10_000), Some(Vec::new()));
        assert_eq!(scan.poll(13_000), None, "nothing changed");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t-1.md");
        fs::write(&path, "---\ntitle: aaa\n---\n").unwrap();
        assert_eq!(scan.poll(14_000), None, "within 2 s of the last look");
        let tasks = scan.poll(15_000).unwrap();
        assert_eq!(tasks[0].title, "aaa");
        assert_eq!(scan.poll(17_000), None);
        // The same length, a new mtime.
        fs::write(&path, "---\ntitle: bbb\n---\n").unwrap();
        let later = SystemTime::now() + Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let tasks = scan.poll(19_000).unwrap();
        assert_eq!(tasks[0].title, "bbb");
        assert_eq!(
            tasks[0].updated,
            later.duration_since(UNIX_EPOCH).unwrap().as_millis() as Millis
        );
        fs::remove_file(&path).unwrap();
        assert_eq!(scan.poll(21_000), Some(Vec::new()));
        assert_eq!(scan.poll(23_000), None);
    }
}
