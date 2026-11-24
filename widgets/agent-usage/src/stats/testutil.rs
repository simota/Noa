//! Test helpers: a temp tree laid out like a home and a data dir, the
//! synthetic fixtures in `testdata/`, and sums over an index.
//!
//! Roots are built by hand (never `Roots::under`, which honours
//! `CLAUDE_CONFIG_DIR` / `CODEX_HOME` and could reach real logs).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use super::aggregate::file_agent;
use super::index::{Index, Indexer, fnv};
use super::types::Counters;
use crate::model::{Agent, Unix};
use crate::paths::Roots;
use crate::time::parse_rfc3339;

/// The home the fixtures' absolute paths live under (shown as `~`).
pub const FIXTURE_HOME: &str = "/home/synthetic";
/// mtime given to the fixtures' agy `.db` files: 2026-11-22T12:00:00Z.
pub const DB_MTIME: Unix = 1_795_348_800;
/// Tokens in the Claude fixtures (main session and subagent).
pub const CLAUDE_FIXTURE_TOKENS: u64 = 1550 + 2050 + 16 + 16;
/// Tokens in the Codex fixtures (main thread and the file without records).
pub const CODEX_FIXTURE_TOKENS: u64 = 2065 + 1295;

const BASE: &str = "2026-11-20T00:00:00Z";

pub struct Tree {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub own: PathBuf,
}

/// Where a `testdata/` path lands in the tree.
fn mapped(rel: &str) -> String {
    for (from, to) in [
        ("claude/", "home/.claude/"),
        ("codex/", "home/.codex/"),
        ("agy/", "home/.gemini/antigravity-cli/"),
        ("ctl/", "data/noa/ctl/"),
    ] {
        if let Some(rest) = rel.strip_prefix(from) {
            return format!("{to}{rest}");
        }
    }
    panic!("unmapped fixture path {rel}");
}

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/stats/testdata")
}

fn fixture_files(dir: &Path, rel: &str, out: &mut Vec<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().into_string().unwrap();
        let rel = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        if e.file_type().unwrap().is_dir() {
            fixture_files(&e.path(), &rel, out);
        } else {
            out.push(rel);
        }
    }
}

impl Tree {
    pub fn new() -> Tree {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let own = root.join("data/noa/agent-usage");
        Tree {
            _dir: dir,
            root,
            own,
        }
    }

    /// Every fixture under `testdata/`, copied into a new tree.
    pub fn fixtures() -> Tree {
        let mut all = Vec::new();
        fixture_files(&testdata(), "", &mut all);
        let t = Tree::new();
        for rel in &all {
            t.copy_fixture(rel);
        }
        t
    }

    /// Where the fixture `rel` (a path under `testdata/`) is copied to.
    pub fn fixture_path(&self, rel: &str) -> PathBuf {
        self.root.join(mapped(rel))
    }

    pub fn copy_fixture(&self, rel: &str) {
        let to = self.fixture_path(rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        let bytes = std::fs::read(testdata().join(rel)).unwrap();
        if rel.ends_with(".db") {
            std::fs::write(&to, bytes).unwrap();
            set_mtime(&to, DB_MTIME);
        } else {
            write(&to, &bytes);
        }
    }

    pub fn roots(&self) -> Roots {
        let home = self.root.join("home");
        Roots {
            claude_projects: Some(home.join(".claude/projects")),
            claude_credentials: Some(home.join(".claude/.credentials.json")),
            codex_sessions: vec![
                home.join(".codex/sessions"),
                home.join(".codex/archived_sessions"),
            ],
            agy_dir: Some(home.join(".gemini/antigravity-cli")),
            ctl_dir: Some(self.root.join("data/noa/ctl")),
            own_dir: Some(self.own.clone()),
        }
    }

    pub fn indexer(&self) -> Indexer {
        Indexer::new(
            self.roots(),
            Some(PathBuf::from(FIXTURE_HOME)),
            Arc::new(AtomicBool::new(false)),
        )
    }

    /// A Claude log path (its directories created); `name` may hold
    /// directories (`<session>/subagents/agent-x.jsonl`).
    pub fn claude_file(&self, project_dir: &str, name: &str) -> PathBuf {
        let path = self
            .root
            .join("home/.claude/projects")
            .join(project_dir)
            .join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    /// A Codex rollout path under `sessions/` (its directories created).
    pub fn codex_file(&self, name: &str) -> PathBuf {
        let dir = self.root.join("home/.codex/sessions/2026/11/21");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// The agy history path (its directories created).
    pub fn agy_history(&self) -> PathBuf {
        let dir = self.root.join("home/.gemini/antigravity-cli");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.jsonl")
    }
}

fn set_mtime(path: &Path, secs: Unix) {
    let f = std::fs::File::options().append(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(secs as u64))
        .unwrap();
}

/// Each write gets a later mtime than the last, so that a rewrite within one
/// tick of the file system clock still reads as changed.
fn bump_mtime(path: &Path) {
    static NEXT: AtomicI64 = AtomicI64::new(1_790_000_000);
    set_mtime(path, NEXT.fetch_add(1, Ordering::Relaxed));
}

pub fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    bump_mtime(path);
}

pub fn append(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut f = std::fs::File::options().append(true).open(path).unwrap();
    f.write_all(bytes).unwrap();
    drop(f);
    bump_mtime(path);
}

fn stamp(offset: i64) -> String {
    let at = parse_rfc3339(BASE).unwrap() + offset;
    let days = at.div_euclid(86_400);
    let secs = at.rem_euclid(86_400);
    // Days from 2026-11-20; the generated logs stay within that month.
    let base_day = parse_rfc3339(BASE).unwrap().div_euclid(86_400);
    format!(
        "2026-11-{:02}T{:02}:{:02}:{:02}Z",
        20 + days - base_day,
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// A user prompt at `BASE + offset` (no reply).
pub fn claude_turn_at(offset: i64) -> Vec<u8> {
    format!(
        r#"{{"type":"user","timestamp":"{}","sessionId":"sess-gen","cwd":"/home/synthetic/gen","message":{{"role":"user","content":"synthetic prompt"}}}}"#,
        stamp(offset)
    )
    .into_bytes()
    .into_iter()
    .chain(*b"\n")
    .collect()
}

/// An assistant reply at an RFC 3339 instant with `output` output tokens.
pub fn claude_reply_on(ts: &str, id: &str, output: u64) -> Vec<u8> {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"sess-gen","cwd":"/home/synthetic/gen","message":{{"id":"{id}","model":"claude-opus-5-5","content":[{{"type":"text","text":"synthetic reply"}}],"usage":{{"input_tokens":0,"output_tokens":{output}}}}}}}"#
    )
    .into_bytes()
    .into_iter()
    .chain(*b"\n")
    .collect()
}

/// A prompt at `BASE + offset` and its reply 5 s later.
pub fn claude_turn_and_reply(offset: i64, id: &str, output: u64) -> Vec<u8> {
    let mut out = claude_turn_at(offset);
    out.extend(claude_reply_on(&stamp(offset + 5), id, output));
    out
}

/// Indexes only the given fixtures (paths under `testdata/`).
pub fn index_fixture_files(rels: &[&str]) -> Index {
    let t = Tree::new();
    for rel in rels {
        t.copy_fixture(rel);
    }
    let mut ix = t.indexer();
    ix.pass(&mut |_| {});
    ix.index().clone()
}

fn add(a: &mut Counters, b: &Counters) {
    a.turns += b.turns;
    a.messages += b.messages;
    a.tool_calls += b.tool_calls;
    a.active_secs += b.active_secs;
    a.tokens.add(&b.tokens);
}

/// Counters summed over every file and bucket the report would use, active
/// time included.
pub fn sum(ix: &Index) -> Counters {
    sum_where(ix, |_| true)
}

pub fn sum_agent(ix: &Index, agent: Agent) -> Counters {
    sum_where(ix, |a| a == agent)
}

fn sum_where(ix: &Index, keep: impl Fn(Agent) -> bool) -> Counters {
    let mut c = Counters::default();
    for e in ix.files.values() {
        let fallback = if e.data.has_records {
            &[][..]
        } else {
            &e.data.fallback[..]
        };
        let active = &e.data.unlinked_active;
        for b in e.data.buckets.iter().chain(fallback).chain(active) {
            if keep(b.agent) {
                add(&mut c, &b.c);
            }
        }
    }
    for b in ix.sessions.values().flat_map(|s| &s.active) {
        if keep(b.agent) {
            add(&mut c, &b.c);
        }
    }
    c
}

/// Lines of JSON, each followed by `\n`.
pub fn jsonl(lines: &[String]) -> Vec<u8> {
    lines
        .iter()
        .flat_map(|l| format!("{l}\n").into_bytes())
        .collect()
}

/// Tool counts, most used first, then by name.
pub fn tools(ix: &Index) -> Vec<(String, u64)> {
    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    for e in ix.files.values() {
        if file_agent(e.kind).is_none() {
            continue;
        }
        for t in &e.data.tools {
            *by.entry(ix.string(t.name).to_owned()).or_default() += t.count;
        }
    }
    let mut v: Vec<_> = by.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// Total tokens per non-empty model name, by name.
pub fn tokens_by_model(ix: &Index) -> Vec<(String, u64)> {
    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    for e in ix.files.values() {
        for b in &e.data.buckets {
            if b.model != 0 {
                *by.entry(ix.string(b.model).to_owned()).or_default() += b.c.tokens.total();
            }
        }
    }
    by.into_iter().collect()
}

pub fn models(ix: &Index) -> Vec<String> {
    tokens_by_model(ix).into_iter().map(|(m, _)| m).collect()
}

/// Every file below `root`: relative path → (length, mtime, content hash).
pub fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, (u64, u128, u64)> {
    fn go(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (u64, u128, u64)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() {
                go(root, &p, out);
            } else {
                let mtime = meta
                    .modified()
                    .unwrap()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let hash = fnv(&std::fs::read(&p).unwrap());
                out.insert(
                    p.strip_prefix(root).unwrap().to_path_buf(),
                    (meta.len(), mtime, hash),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    go(root, root, &mut out);
    out
}
