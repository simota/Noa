//! Reading a source as I3 prescribes (lstat, 1 MiB cap, UTF-8, lines), its
//! anchors, and the bodies of the functions the rules look into.
//!
//! The rules are written as regular expressions in the spec; this crate has
//! no regex engine (its dependencies are fixed, Decision 5), so each pattern
//! is matched by hand in [`pat`]. Every hand matcher names the expression it
//! stands for.

use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

/// Larger sources are `unknown` (`too large`).
pub const MAX_BYTES: u64 = 1_048_576;

/// One file the scan reads: its lines, or why it could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Relative to the repo, with `/`.
    pub path: String,
    pub error: Option<String>,
    pub lines: Vec<String>,
    /// The line of its first anchor; 1 for a source without one.
    pub anchor: u32,
}

/// `rel` (with `/`) under `repo`.
pub fn abs_path(repo: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(repo.to_path_buf(), |p, part| p.join(part))
}

/// Opens a regular file without following a link in its last component.
pub fn open_nofollow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

/// The bytes of `path` if it is a regular file of at most [`MAX_BYTES`];
/// else the I3 error string.
pub fn read_capped(path: &Path) -> Result<Vec<u8>, &'static str> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "missing")?;
    if !meta.file_type().is_file() {
        return Err("not a regular file");
    }
    if meta.len() > MAX_BYTES {
        return Err("too large");
    }
    let file = open_nofollow(path).map_err(|_| "missing")?;
    let mut raw = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "missing")?;
    if raw.len() as u64 > MAX_BYTES {
        return Err("too large");
    }
    Ok(raw)
}

/// Splits on `\n`, drops the empty piece after a final `\n` and a `\r` at
/// the end of each line.
pub fn split_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
        .collect()
}

/// The 1-based line of byte `offset` in `text`.
pub fn line_of(text: &str, offset: usize) -> u32 {
    text.as_bytes()[..offset]
        .iter()
        .filter(|b| **b == b'\n')
        .count() as u32
        + 1
}

impl Source {
    fn failed(path: &str, error: &str) -> Source {
        Source {
            path: path.to_owned(),
            error: Some(error.to_owned()),
            lines: Vec::new(),
            anchor: 1,
        }
    }

    /// Reads `rel`; a source that cannot be used has `error` set.
    pub fn load(repo: &Path, rel: &str) -> Source {
        let raw = match read_capped(&abs_path(repo, rel)) {
            Ok(raw) => raw,
            Err(e) => return Source::failed(rel, e),
        };
        let Ok(text) = String::from_utf8(raw) else {
            return Source::failed(rel, "not UTF-8");
        };
        Source {
            path: rel.to_owned(),
            error: None,
            lines: split_lines(&text),
            anchor: 1,
        }
    }

    /// A source made of `error` alone (the `widgets` folder).
    pub fn bare(path: &str, error: Option<&str>) -> Source {
        Source {
            path: path.to_owned(),
            error: error.map(str::to_owned),
            lines: Vec::new(),
            anchor: 1,
        }
    }

    pub fn ok(&self) -> bool {
        self.error.is_none()
    }

    /// The lines joined by `\n`, for the patterns that span lines.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// The first line containing `needle`.
    pub fn first_line(&self, needle: &str) -> Option<u32> {
        self.numbered()
            .find(|(_, l)| l.contains(needle))
            .map(|(n, _)| n)
    }

    /// `(number, line)` from 1.
    pub fn numbered(&self) -> impl Iterator<Item = (u32, &str)> {
        self.lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i as u32 + 1, l.as_str()))
    }

    /// The function body of `fn NAME` (I3): from the first line containing
    /// `fn NAME(` to the first later line that is its indentation then `}`;
    /// `None` when there is no such line. Lines are numbered.
    pub fn body(&self, name: &str) -> Option<Vec<(u32, &str)>> {
        let head = format!("fn {name}(");
        let start = self.lines.iter().position(|l| l.contains(&head))?;
        let line = &self.lines[start];
        let indent = &line[..line.len() - pat::trim_ws_start(line).len()];
        let close = format!("{indent}}}");
        let end = (start + 1..self.lines.len()).find(|i| self.lines[*i] == close)?;
        Some(
            (start..=end)
                .map(|i| (i as u32 + 1, self.lines[i].as_str()))
                .collect(),
        )
    }

    pub fn fail_anchor(&mut self, anchor: &str) {
        self.error = Some(format!("anchor not found: {anchor}"));
    }

    /// Anchors on `fn NAME` for each name in order; the first sets the
    /// anchor line. A function whose body cannot be bounded is not found.
    pub fn anchor_functions(&mut self, names: &[&str]) {
        for (i, name) in names.iter().enumerate() {
            let anchor = format!("fn {name}");
            match self.first_line(&anchor) {
                Some(n) if self.body(name).is_some() => {
                    if i == 0 {
                        self.anchor = n;
                    }
                }
                _ => {
                    self.fail_anchor(&anchor);
                    return;
                }
            }
        }
    }
}

/// Hand matchers for the spec's regular expressions. `\s` is
/// [`char::is_whitespace`] and `\w` is alphanumeric or `_`, as in Python's
/// `re` on `str`.
pub mod pat {
    pub fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    pub fn trim_ws_start(s: &str) -> &str {
        s.trim_start_matches(char::is_whitespace)
    }

    pub fn trim_ws_end(s: &str) -> &str {
        s.trim_end_matches(char::is_whitespace)
    }

    /// The longest prefix of `s` whose characters satisfy `f`, and the rest.
    pub fn run(s: &str, f: impl Fn(char) -> bool) -> (&str, &str) {
        let end = s.find(|c: char| !f(c)).unwrap_or(s.len());
        s.split_at(end)
    }

    /// A non-empty [`run`].
    pub fn run1(s: &str, f: impl Fn(char) -> bool) -> Option<(&str, &str)> {
        let (head, rest) = run(s, f);
        (!head.is_empty()).then_some((head, rest))
    }

    pub fn lower_kebab(c: char) -> bool {
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
    }

    pub fn upper_snake(c: char) -> bool {
        c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'
    }

    /// Byte offsets of every occurrence of `needle` in `hay`, overlapping.
    pub fn occurrences<'a>(hay: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
        hay.char_indices()
            .map(|(i, _)| i)
            .filter(move |i| hay[*i..].starts_with(needle))
    }

    /// `re.search(prefix + "(" + class + "+)", line)`: the run after the
    /// first `prefix` that is followed by at least one character of `class`.
    pub fn search_after<'a>(
        line: &'a str,
        prefix: &str,
        class: impl Fn(char) -> bool + Copy,
    ) -> Option<(&'a str, &'a str)> {
        occurrences(line, prefix).find_map(|i| run1(&line[i + prefix.len()..], class))
    }

    /// `pub\(crate\) const ([A-Z][A-Z0-9_]*): HelperProgram = HelperProgram \{(.*?)\};`
    /// with `.` matching newlines: `(name, body, byte offset of body)` of
    /// each non-overlapping match, left to right.
    pub fn helper_consts(text: &str) -> Vec<(&str, &str, usize)> {
        const HEAD: &str = "pub(crate) const ";
        const MID: &str = ": HelperProgram = HelperProgram {";
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(at) = text[from..].find(HEAD).map(|i| from + i) {
            let after = &text[at + HEAD.len()..];
            let matched = after
                .chars()
                .next()
                .filter(char::is_ascii_uppercase)
                .and_then(|_| {
                    let (name, rest) = run(after, upper_snake);
                    let body_at = at + HEAD.len() + name.len() + MID.len();
                    let body = rest.strip_prefix(MID)?;
                    let len = body.find("};")?;
                    Some((name, &text[body_at..body_at + len], body_at))
                });
            match matched {
                Some((name, body, body_at)) => {
                    out.push((name, body, body_at));
                    from = body_at + body.len() + 2;
                }
                None => from = at + HEAD.len(),
            }
        }
        out
    }

    /// `(\w+): "([^"]*)"`: `(key, value, byte offset of the match)` of each
    /// non-overlapping match.
    pub fn fields(text: &str) -> Vec<(&str, &str, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < text.len() {
            let rest = &text[i..];
            let Some(c) = rest.chars().next() else { break };
            if !is_word(c) {
                i += c.len_utf8();
                continue;
            }
            let (key, after) = run(rest, is_word);
            if let Some(value) = after.strip_prefix(": \"")
                && let Some(len) = value.find('"')
            {
                out.push((key, &value[..len], i));
                i += key.len() + 3 + len + 1;
            } else {
                i += key.len();
            }
        }
        out
    }

    /// `^name = "([^"]+)"$`.
    pub fn toml_name(line: &str) -> Option<&str> {
        let v = line.strip_prefix("name = \"")?.strip_suffix('"')?;
        (!v.is_empty() && !v.contains('"')).then_some(v)
    }

    /// `^for widget in (.+); do$`: the list.
    pub fn for_widget(line: &str) -> Option<&str> {
        let v = line.strip_prefix("for widget in ")?.strip_suffix("; do")?;
        (!v.is_empty()).then_some(v)
    }

    /// `^\s*"([a-z0-9-]+):([^"]+)"\s*$`: `(crate, installed name)`.
    pub fn package_item(line: &str) -> Option<(&str, &str)> {
        let inner = trim_ws_end(trim_ws_start(line))
            .strip_prefix('"')?
            .strip_suffix('"')?;
        let (name, rest) = run1(inner, lower_kebab)?;
        let installed = rest.strip_prefix(':')?;
        (!installed.is_empty() && !installed.contains('"')).then_some((name, installed))
    }

    /// `AppCommand::([A-Za-z0-9]+)\s*=>\s*\{?\s*self\.open_helper_program\(\s*(crate::helper_program::C)\s*\)`
    /// searched in `text`: the variant and the byte offset of group 2.
    pub fn dispatch<'a>(text: &'a str, konst: &str) -> Option<(&'a str, usize)> {
        let target = format!("crate::helper_program::{konst}");
        occurrences(text, "AppCommand::").find_map(|at| {
            let after = &text[at + "AppCommand::".len()..];
            let (variant, rest) = run1(after, |c| c.is_ascii_alphanumeric())?;
            let rest = trim_ws_start(rest).strip_prefix("=>")?;
            let rest = trim_ws_start(rest);
            let rest = trim_ws_start(rest.strip_prefix('{').unwrap_or(rest));
            let rest = rest.strip_prefix("self.open_helper_program(")?;
            let rest = trim_ws_start(rest);
            let group = text.len() - rest.len();
            let rest = trim_ws_start(rest.strip_prefix(&target)?);
            rest.starts_with(')').then_some((variant, group))
        })
    }

    /// `^\s*AppCommand::V,\s*$`.
    pub fn palette_entry(line: &str, variant: &str) -> bool {
        trim_ws_end(trim_ws_start(line)) == format!("AppCommand::{variant},")
    }

    /// `Self::V => "([a-z0-9-]+\.open)"`, searched.
    pub fn fixed_action<'a>(line: &'a str, variant: &str) -> Option<&'a str> {
        let prefix = format!("Self::{variant} => \"");
        occurrences(line, &prefix).find_map(|i| {
            let start = i + prefix.len();
            let (name, rest) = run1(&line[start..], lower_kebab)?;
            rest.starts_with(".open\"")
                .then(|| &line[start..start + name.len() + ".open".len()])
        })
    }

    /// `^(\w+)=widgets/W/Cargo\.toml$`: the variable.
    pub fn manifest_var<'a>(line: &'a str, widget: &str) -> Option<&'a str> {
        let (var, rest) = run1(line, is_word)?;
        (rest == format!("=widgets/{widget}/Cargo.toml")).then_some(var)
    }

    /// `^\s*(?:in_part (?:core|widgets) && )?run (\S+) (widget_check|usage_check) "\$X"\s*$`: the check.
    pub fn full_run<'a>(line: &'a str, var: &str) -> Option<&'a str> {
        let line = trim_ws_start(line);
        let line = line
            .strip_prefix("in_part core && ")
            .or_else(|| line.strip_prefix("in_part widgets && "))
            .unwrap_or(line);
        let rest = line.strip_prefix("run ")?;
        let (_, rest) = run1(rest, |c| !c.is_whitespace())?;
        check_of(rest.strip_prefix(' ')?, var)
    }

    /// `(widget_check|usage_check) "\$X"\s*$` at the start of `rest`.
    fn check_of<'a>(rest: &'a str, var: &str) -> Option<&'a str> {
        let (check, rest) = run1(rest, is_word)?;
        if check != "widget_check" && check != "usage_check" {
            return None;
        }
        (trim_ws_end(rest) == format!(" \"${var}\"")).then_some(check)
    }

    /// `^\s*case \$path in ([^)]*)\)`: the alternatives.
    pub fn case_path(line: &str) -> Option<&str> {
        let rest = trim_ws_start(line).strip_prefix("case $path in ")?;
        let end = rest.find(')')?;
        Some(&rest[..end])
    }

    /// `^\s*\[ "\$\w+" = 1 \] && run \S+ (widget_check|usage_check) "\$X"\s*$`.
    pub fn changed_run(line: &str, var: &str) -> bool {
        let Some(rest) = trim_ws_start(line).strip_prefix("[ \"$") else {
            return false;
        };
        let Some((_, rest)) = run1(rest, is_word) else {
            return false;
        };
        let Some(rest) = rest.strip_prefix("\" = 1 ] && run ") else {
            return false;
        };
        let Some((_, rest)) = run1(rest, |c| !c.is_whitespace()) else {
            return false;
        };
        rest.strip_prefix(' ')
            .and_then(|r| check_of(r, var))
            .is_some()
    }

    /// `^(\w+)="\$TARGET_DIR/W"$`: the variable.
    pub fn binary_var<'a>(line: &'a str, widget: &str) -> Option<&'a str> {
        let (var, rest) = run1(line, is_word)?;
        (rest == format!("=\"$TARGET_DIR/{widget}\"")).then_some(var)
    }

    /// `^cp "\$B" "\$CONTENTS/Helpers/([^"]+)"$`: the installed name.
    pub fn copy_name<'a>(line: &'a str, var: &str) -> Option<&'a str> {
        let prefix = format!("cp \"${var}\" \"$CONTENTS/Helpers/");
        let name = line.strip_prefix(&prefix)?.strip_suffix('"')?;
        (!name.is_empty() && !name.contains('"')).then_some(name)
    }

    /// `^\s*"<key>"\s*:`.
    pub fn json_key_line(line: &str, key: &str) -> bool {
        trim_ws_start(line)
            .strip_prefix(&format!("\"{key}\""))
            .is_some_and(|r| trim_ws_start(r).starts_with(':'))
    }
}

#[cfg(test)]
mod tests {
    use super::pat::*;
    use super::*;

    #[test]
    fn lines_drop_cr_and_the_final_empty_piece() {
        assert_eq!(split_lines("a\r\nb\n"), ["a", "b"]);
        assert_eq!(split_lines("a\n\n"), ["a", ""]);
        assert_eq!(split_lines(""), Vec::<String>::new());
        assert_eq!(split_lines("x"), ["x"]);
        assert_eq!(line_of("a\nb\nc", 4), 3);
    }

    #[test]
    fn reading_errors_are_the_spec_strings() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        assert_eq!(Source::load(repo, "nope").error.as_deref(), Some("missing"));
        std::fs::create_dir(repo.join("d")).unwrap();
        assert_eq!(
            Source::load(repo, "d").error.as_deref(),
            Some("not a regular file")
        );
        std::fs::write(repo.join("t"), "ok\n").unwrap();
        std::os::unix::fs::symlink(repo.join("t"), repo.join("l")).unwrap();
        assert_eq!(
            Source::load(repo, "l").error.as_deref(),
            Some("not a regular file")
        );
        std::fs::write(repo.join("big"), vec![b'a'; MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(
            Source::load(repo, "big").error.as_deref(),
            Some("too large")
        );
        std::fs::write(repo.join("exact"), vec![b'a'; MAX_BYTES as usize]).unwrap();
        assert!(Source::load(repo, "exact").ok());
        std::fs::write(repo.join("bin"), [0xff, 0xfe]).unwrap();
        assert_eq!(
            Source::load(repo, "bin").error.as_deref(),
            Some("not UTF-8")
        );
        assert_eq!(Source::load(repo, "t").lines, ["ok"]);
    }

    #[test]
    fn function_bodies_end_at_their_indented_brace() {
        let src = Source {
            path: "x".into(),
            error: None,
            lines: [
                "impl A {",
                "    fn menu_id(self) {",
                "        match self {",
                "        }",
                "    }",
                "    fn from_menu_id(x) {",
                "    }",
                "}",
            ]
            .map(String::from)
            .to_vec(),
            anchor: 1,
        };
        let body = src.body("menu_id").unwrap();
        assert_eq!(body.first().unwrap().0, 2);
        assert_eq!(body.last().unwrap().0, 5);
        assert_eq!(src.body("from_menu_id").unwrap().len(), 2);
        assert!(src.body("nope").is_none());
        let mut s = src.clone();
        s.anchor_functions(&["from_menu_id", "menu_id"]);
        assert!(s.ok());
        assert_eq!(s.anchor, 6);
        let mut s = src.clone();
        s.anchor_functions(&["menu_id", "launch_args"]);
        assert_eq!(s.error.as_deref(), Some("anchor not found: fn launch_args"));
    }

    #[test]
    fn helper_consts_and_fields() {
        let text = "pub(crate) const TASKS: HelperProgram = HelperProgram {\n    title: \"Tasks\",\n    bundled: \"Noa Tasks\",\n    crate_name: \"tasks\",\n};\npub(crate) const bad: HelperProgram = HelperProgram {};\npub(crate) const B2: HelperProgram = HelperProgram { crate_name: \"b\" };";
        let consts = helper_consts(text);
        assert_eq!(consts.len(), 2);
        assert_eq!(consts[0].0, "TASKS");
        assert_eq!(consts[1].0, "B2");
        let f = fields(consts[0].1);
        assert_eq!(
            f.iter().map(|(k, v, _)| (*k, *v)).collect::<Vec<_>>(),
            [
                ("title", "Tasks"),
                ("bundled", "Noa Tasks"),
                ("crate_name", "tasks")
            ]
        );
        let at = consts[0].2 + f[2].2;
        assert_eq!(line_of(text, at), 4);
    }

    #[test]
    fn dispatch_matches_one_line_and_braced_arms() {
        let text = "match c {\n    AppCommand::OpenTasks => self.open_helper_program(crate::helper_program::TASKS),\n    AppCommand::OpenX => {\n        self.open_helper_program(crate::helper_program::X)\n    }\n    AppCommand::OpenY => self.open_helper_program(crate::helper_program::Y_Z),\n}";
        let (v, at) = dispatch(text, "TASKS").unwrap();
        assert_eq!((v, line_of(text, at)), ("OpenTasks", 2));
        let (v, at) = dispatch(text, "X").unwrap();
        assert_eq!((v, line_of(text, at)), ("OpenX", 4));
        assert!(dispatch(text, "Y").is_none());
        assert!(dispatch(text, "Y_Z").is_some());
    }

    #[test]
    fn line_patterns() {
        assert_eq!(toml_name("name = \"tasks\""), Some("tasks"));
        assert_eq!(toml_name("name = \"\""), None);
        assert_eq!(toml_name(" name = \"x\""), None);
        assert_eq!(for_widget("for widget in a b c; do"), Some("a b c"));
        assert_eq!(
            package_item("  \"agent-usage:Noa Agent Usage\"  "),
            Some(("agent-usage", "Noa Agent Usage"))
        );
        assert_eq!(package_item("\"Agent:Noa\""), None);
        assert!(palette_entry(
            "        AppCommand::OpenTasks, ",
            "OpenTasks"
        ));
        assert!(!palette_entry("AppCommand::OpenTasks, // x", "OpenTasks"));
        assert_eq!(
            fixed_action("    Self::OpenTasks => \"tasks.open\",", "OpenTasks"),
            Some("tasks.open")
        );
        assert_eq!(
            fixed_action("Self::OpenTasks => \"tasks.close\"", "OpenTasks"),
            None
        );
        assert_eq!(
            search_after(
                "AppCommand::OpenTasks => Self::OPEN_TASKS_MENU_ID,",
                "AppCommand::OpenTasks => Self::",
                upper_snake
            )
            .map(|x| x.0),
            Some("OPEN_TASKS_MENU_ID")
        );
        assert_eq!(
            manifest_var("tasks=widgets/tasks/Cargo.toml", "tasks"),
            Some("tasks")
        );
        assert_eq!(
            full_run("  run tasks usage_check \"$tasks\"", "tasks"),
            Some("usage_check")
        );
        assert_eq!(full_run("  run tasks other \"$tasks\"", "tasks"), None);
        assert!(changed_run(
            "  [ \"$tsk\" = 1 ] && run tasks usage_check \"$tasks\"",
            "tasks"
        ));
        assert_eq!(
            case_path("    case $path in widgets/tasks/* | skills/x/*) tsk=1 ;; esac"),
            Some("widgets/tasks/* | skills/x/*")
        );
        assert_eq!(
            binary_var("TASKS_BIN=\"$TARGET_DIR/tasks\"", "tasks"),
            Some("TASKS_BIN")
        );
        assert_eq!(
            copy_name(
                "cp \"$TASKS_BIN\" \"$CONTENTS/Helpers/Noa Tasks\"",
                "TASKS_BIN"
            ),
            Some("Noa Tasks")
        );
        assert!(json_key_line("    \"tasks\" : {", "tasks"));
        assert!(!json_key_line("    \"tasks2\": {", "tasks"));
    }
}
