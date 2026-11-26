//! The widget's own files, never in the repo (I5):
//!
//! ```text
//! <data dir>/noa/scaffold/<repo-key>/
//!   scan.json           the last scan
//!   improvements.json   every candidate ever found, merged by id
//!   observations.jsonl  links and skips, append-only
//! ```
//!
//! Verify and Review sessions are linked by id and folder path; their
//! content is never copied (Decision 8).

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::hash;
use crate::model::{Improvement, Observation, Subject, candidate_id};
use crate::scan::Report;
use crate::scan::source::{open_nofollow, read_capped};
use crate::time::{self, Millis};

pub const SCAN_FILE: &str = "scan.json";
pub const IMPROVEMENTS_FILE: &str = "improvements.json";
pub const OBSERVATIONS_FILE: &str = "observations.jsonl";

/// `<data dir>`: `dirs::data_dir()`, as `noa_config::session_state_path`.
pub fn data_dir() -> Option<PathBuf> {
    dirs::data_dir()
}

/// `<dir name>-<FNV-1a 64 hex of the canonical repo path>`.
pub fn repo_key(repo: &Path) -> String {
    let canonical = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.components().collect());
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_owned());
    format!(
        "{name}-{}",
        hash::hex(canonical.as_os_str().as_encoded_bytes())
    )
}

/// A hidden, unused name beside `path` for a temp file.
pub fn temp_sibling(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{nanos}-{n}.tmp"))
}

/// Writes `bytes` to `path` through a temp file and a rename.
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = temp_sibling(path);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanFile {
    pub schema: u32,
    pub at: Millis,
    pub commit: Option<String>,
    pub report: Report,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ImprovementsFile {
    schema: u32,
    improvements: Vec<Improvement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    Verify,
    Review,
    Task,
}

impl LinkKind {
    pub fn label(self) -> &'static str {
        match self {
            LinkKind::Verify => "検証",
            LinkKind::Review => "レビュー",
            LinkKind::Task => "タスク",
        }
    }
}

/// One line of `observations.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Linked {
        subject: Subject,
        kind: LinkKind,
        r#ref: String,
        path: PathBuf,
        at: Millis,
    },
    Unlinked {
        subject: Subject,
        kind: LinkKind,
        r#ref: String,
        at: Millis,
    },
    Skipped {
        subject: Subject,
        reason: String,
        commit: Option<String>,
        at: Millis,
    },
}

/// The standing observations: each link not removed since, and each skip,
/// in log order.
pub fn fold(events: &[Event]) -> Vec<Observation> {
    let mut out: Vec<Observation> = Vec::new();
    for e in events {
        match e {
            Event::Linked {
                subject,
                kind,
                r#ref,
                path,
                at,
            } => {
                let (subject, session, path, at) =
                    (subject.clone(), r#ref.clone(), path.clone(), *at);
                if out
                    .iter()
                    .any(|o| link_of(o) == Some((&subject, *kind, &session)))
                {
                    continue;
                }
                out.push(match kind {
                    LinkKind::Verify => Observation::Verify {
                        subject,
                        session,
                        path,
                        at,
                    },
                    LinkKind::Review => Observation::Review {
                        subject,
                        session,
                        path,
                        at,
                    },
                    LinkKind::Task => Observation::Task {
                        subject,
                        id: session,
                        path,
                        at,
                    },
                });
            }
            Event::Unlinked {
                subject,
                kind,
                r#ref,
                ..
            } => out.retain(|o| link_of(o) != Some((subject, *kind, r#ref))),
            Event::Skipped {
                subject,
                reason,
                commit,
                at,
            } => out.push(Observation::Skipped {
                subject: subject.clone(),
                reason: reason.clone(),
                commit: commit.clone(),
                at: *at,
            }),
        }
    }
    out
}

/// `(subject, kind, ref)` of a link observation.
pub fn link_of(o: &Observation) -> Option<(&Subject, LinkKind, &String)> {
    match o {
        Observation::Verify {
            subject, session, ..
        } => Some((subject, LinkKind::Verify, session)),
        Observation::Review {
            subject, session, ..
        } => Some((subject, LinkKind::Review, session)),
        Observation::Task { subject, id, .. } => Some((subject, LinkKind::Task, id)),
        Observation::Skipped { .. } => None,
    }
}

/// Merges a scan into the stored candidates by id (I5): a seen id updates
/// its evidence and `last_seen` and clears `resolved`; a stored id the scan
/// no longer finds is resolved at `now`. Nothing is removed or duplicated.
/// A scan with an unknown source resolves nothing: what it could not read
/// it cannot have found fixed.
pub fn merge(
    stored: Vec<Improvement>,
    report: &Report,
    identity: &str,
    now: Millis,
) -> Vec<Improvement> {
    let mut out = stored;
    let mut seen = Vec::new();
    for f in &report.findings {
        let id = candidate_id(identity, &f.rule, &f.widget, &f.target);
        seen.push(id.clone());
        match out.iter_mut().find(|i| i.id == id) {
            Some(i) => {
                i.evidence = f.evidence.clone();
                i.last_seen = now;
                i.resolved = None;
            }
            None => out.push(Improvement {
                id,
                rule: f.rule.clone(),
                widget: f.widget.clone(),
                target: f.target.clone(),
                evidence: f.evidence.clone(),
                first_seen: now,
                last_seen: now,
                resolved: None,
            }),
        }
    }
    if !report.has_unknown() {
        for i in &mut out {
            if !seen.contains(&i.id) && i.resolved.is_none() {
                i.resolved = Some(now);
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// One repo's folder under `<data dir>/noa/scaffold`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    pub data_dir: PathBuf,
    pub dir: PathBuf,
}

impl Store {
    pub fn new(data_dir: &Path, repo: &Path) -> Store {
        Store {
            data_dir: data_dir.to_owned(),
            dir: data_dir.join("noa").join("scaffold").join(repo_key(repo)),
        }
    }

    fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)
    }

    fn read_json<T: for<'de> Deserialize<'de>>(&self, name: &str) -> Option<T> {
        let bytes = read_capped(&self.dir.join(name)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn load_scan(&self) -> Option<ScanFile> {
        self.read_json(SCAN_FILE)
    }

    pub fn save_scan(&self, scan: &ScanFile) -> std::io::Result<()> {
        self.ensure()?;
        let text = serde_json::to_vec(scan).map_err(std::io::Error::other)?;
        replace(&self.dir.join(SCAN_FILE), &text)
    }

    /// Every stored candidate. Only a missing file is an empty history:
    /// one that cannot be read or parsed is an error, so that nothing
    /// overwrites it (I5).
    pub fn improvements(&self) -> Result<Vec<Improvement>, String> {
        let path = self.dir.join(IMPROVEMENTS_FILE);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("{IMPROVEMENTS_FILE} を読めません: {e}")),
            Ok(_) => {}
        }
        let bytes =
            read_capped(&path).map_err(|e| format!("{IMPROVEMENTS_FILE} を読めません: {e}"))?;
        serde_json::from_slice::<ImprovementsFile>(&bytes)
            .map(|f| f.improvements)
            .map_err(|e| format!("{IMPROVEMENTS_FILE} が壊れています: {e}"))
    }

    pub fn save_improvements(&self, list: &[Improvement]) -> std::io::Result<()> {
        self.ensure()?;
        let file = ImprovementsFile {
            schema: 1,
            improvements: list.to_vec(),
        };
        let text = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        replace(&self.dir.join(IMPROVEMENTS_FILE), &text)
    }

    /// Records a scan: `scan.json` and the merged candidates, which it gives.
    /// When the stored candidates cannot be read, nothing is written.
    pub fn record_scan(
        &self,
        report: &Report,
        commit: Option<String>,
        identity: &str,
        now: Millis,
    ) -> std::io::Result<Vec<Improvement>> {
        let stored = self.improvements().map_err(std::io::Error::other)?;
        let merged = merge(stored, report, identity, now);
        self.save_improvements(&merged)?;
        self.save_scan(&ScanFile {
            schema: 1,
            at: now,
            commit,
            report: report.clone(),
        })?;
        Ok(merged)
    }

    /// Every event of the log; a torn or unreadable line is skipped.
    pub fn events(&self) -> Vec<Event> {
        let mut bytes = Vec::new();
        let read = open_nofollow(&self.dir.join(OBSERVATIONS_FILE))
            .and_then(|mut f| f.read_to_end(&mut bytes));
        if read.is_err() {
            return Vec::new();
        }
        // Bytes, not `lines()`: a line torn inside a UTF-8 sequence must
        // not end the read.
        bytes
            .split(|&b| b == b'\n')
            .filter_map(|l| std::str::from_utf8(l).ok())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    pub fn observations(&self) -> Vec<Observation> {
        fold(&self.events())
    }

    pub fn append(&self, event: &Event) -> std::io::Result<()> {
        self.ensure()?;
        let mut line = serde_json::to_string(event).map_err(std::io::Error::other)?;
        line.push('\n');
        let path = self.dir.join(OBSERVATIONS_FILE);
        // A torn last line (a crash mid-write) must not swallow this one.
        let torn = std::fs::read(&path).is_ok_and(|b| !b.is_empty() && !b.ends_with(b"\n"));
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        if torn {
            f.write_all(b"\n")?;
        }
        f.write_all(line.as_bytes())
    }

    /// The folder of a Verify or Review session.
    pub fn session_dir(&self, kind: LinkKind, id: &str) -> Option<PathBuf> {
        let folder = match kind {
            LinkKind::Verify => "verifications",
            LinkKind::Review => "reviews",
            LinkKind::Task => return None,
        };
        Some(self.data_dir.join("noa").join(folder).join(id))
    }

    /// Links a Verify or Review session by id: the id must be one path
    /// component `ver-…` / `rev-…` whose `events.jsonl` exists.
    pub fn link_session(
        &self,
        subject: Subject,
        kind: LinkKind,
        id: &str,
        now: Millis,
    ) -> Result<PathBuf, String> {
        let prefix = match kind {
            LinkKind::Verify => "ver-",
            LinkKind::Review => "rev-",
            LinkKind::Task => return Err("タスクは タスクにする で関連付けます".into()),
        };
        let id = id.trim();
        let valid = id.strip_prefix(prefix).is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        });
        if !valid {
            return Err(format!("{prefix}… の形の id ではありません: {id:?}"));
        }
        let dir = self.session_dir(kind, id).unwrap_or_default();
        let log = std::fs::symlink_metadata(dir.join("events.jsonl"));
        if !log.is_ok_and(|m| m.file_type().is_file()) {
            return Err(format!("{} {id} が見つかりません", kind.label()));
        }
        self.append(&Event::Linked {
            subject,
            kind,
            r#ref: id.to_owned(),
            path: dir.clone(),
            at: now,
        })
        .map_err(|e| e.to_string())?;
        Ok(dir)
    }

    pub fn unlink(
        &self,
        subject: Subject,
        kind: LinkKind,
        id: &str,
        now: Millis,
    ) -> Result<(), String> {
        self.append(&Event::Unlinked {
            subject,
            kind,
            r#ref: id.to_owned(),
            at: now,
        })
        .map_err(|e| e.to_string())
    }

    /// 省略にする: needs a reason.
    pub fn skip(
        &self,
        subject: Subject,
        reason: &str,
        commit: Option<String>,
        now: Millis,
    ) -> Result<(), String> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err("省略の理由を書いてください".into());
        }
        self.append(&Event::Skipped {
            subject,
            reason: reason.to_owned(),
            commit,
            at: now,
        })
        .map_err(|e| e.to_string())
    }
}

/// The current time, for callers that have no fixed clock.
pub fn now() -> Millis {
    time::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Evidence, Finding, SCOPE, SourceReport, SourceState};

    fn report(findings: &[(&str, &str, &str)], unknown: bool) -> Report {
        Report {
            schema: 1,
            scope: SCOPE.into(),
            status: "inconsistent".into(),
            exit: 1,
            sources: vec![SourceReport {
                path: "scripts/verify.sh".into(),
                state: if unknown {
                    SourceState::Unknown
                } else {
                    SourceState::Ok
                },
                error: unknown.then(|| "missing".into()),
            }],
            widgets: vec![],
            findings: findings
                .iter()
                .map(|(r, w, t)| Finding {
                    rule: r.to_string(),
                    widget: w.to_string(),
                    target: t.to_string(),
                    parts: vec![],
                    evidence: vec![Evidence::new("widgets/x/Cargo.toml", 2)],
                })
                .collect(),
            explained: vec![],
        }
    }

    #[test]
    fn rescanning_merges_without_duplicates() {
        let a = ("missing-registration", "g", "launch");
        let b = ("ship-missing", "g", "linux");
        let first = merge(Vec::new(), &report(&[a, b], false), "/r", 10);
        assert_eq!(first.len(), 2);
        let again = merge(first.clone(), &report(&[a, b], false), "/r", 20);
        assert_eq!(again.len(), 2);
        assert!(
            again
                .iter()
                .all(|i| i.first_seen == 10 && i.last_seen == 20)
        );
        // b is fixed: resolved, kept.
        let fixed = merge(again, &report(&[a], false), "/r", 30);
        assert_eq!(fixed.len(), 2);
        let b_id = candidate_id("/r", b.0, b.1, b.2);
        let b_imp = fixed.iter().find(|i| i.id == b_id).unwrap();
        assert_eq!(b_imp.resolved, Some(30));
        // A later scan does not move its resolution; b coming back clears it.
        let later = merge(fixed.clone(), &report(&[a], false), "/r", 40);
        assert_eq!(
            later.iter().find(|i| i.id == b_id).unwrap().resolved,
            Some(30)
        );
        let back = merge(later, &report(&[a, b], false), "/r", 50);
        let b_imp = back.iter().find(|i| i.id == b_id).unwrap();
        assert_eq!((b_imp.resolved, b_imp.first_seen), (None, 10));
        // Sorted by id.
        let ids: Vec<_> = back.iter().map(|i| i.id.clone()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn a_scan_with_an_unknown_source_resolves_nothing() {
        let a = ("missing-registration", "g", "launch");
        let first = merge(Vec::new(), &report(&[a], false), "/r", 10);
        let after = merge(first, &report(&[], true), "/r", 20);
        assert_eq!(after[0].resolved, None);
    }

    #[test]
    fn files_round_trip_and_the_log_folds() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let store = Store::new(data.path(), repo.path());
        assert!(store.dir.starts_with(data.path().join("noa/scaffold")));
        let r = report(&[("no-intent", "g", "registry")], false);
        let merged = store.record_scan(&r, Some("abc".into()), "/r", 5).unwrap();
        assert_eq!(store.improvements(), Ok(merged));
        assert_eq!(store.load_scan().unwrap().report, r);

        let sub = Subject::Asset("a".into());
        let vdir = data.path().join("noa/verifications/ver-1");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("events.jsonl"), "{}\n").unwrap();
        assert!(
            store
                .link_session(sub.clone(), LinkKind::Verify, "ver-2", 1)
                .is_err()
        );
        assert!(
            store
                .link_session(sub.clone(), LinkKind::Verify, "../ver-1", 1)
                .is_err()
        );
        assert!(
            store
                .link_session(sub.clone(), LinkKind::Review, "ver-1", 1)
                .is_err()
        );
        assert_eq!(
            store.link_session(sub.clone(), LinkKind::Verify, "ver-1", 1),
            Ok(vdir.clone())
        );
        store
            .link_session(sub.clone(), LinkKind::Verify, "ver-1", 2)
            .unwrap();
        assert_eq!(store.observations().len(), 1, "a link is not doubled");
        assert!(store.skip(sub.clone(), " ", None, 3).is_err());
        store.skip(sub.clone(), "不要", None, 3).unwrap();
        // A torn line is skipped and does not swallow the next event.
        let log = store.dir.join(OBSERVATIONS_FILE);
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(b"{\"event\":\"lin").unwrap();
        store
            .unlink(sub.clone(), LinkKind::Verify, "ver-1", 4)
            .unwrap();
        let obs = store.observations();
        assert_eq!(obs.len(), 1);
        assert!(matches!(obs[0], Observation::Skipped { .. }));
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.starts_with(
            "{\"event\":\"linked\",\"subject\":{\"asset\":\"a\"},\"kind\":\"verify\",\"ref\":\"ver-1\",\"path\":"
        ));
        assert!(text.contains("{\"event\":\"skipped\",\"subject\":{\"asset\":\"a\"},\"reason\":\"不要\",\"commit\":null,\"at\":3}"));
    }

    #[test]
    fn an_unreadable_history_is_never_overwritten() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let store = Store::new(data.path(), repo.path());
        // Missing: an empty history.
        assert_eq!(store.improvements(), Ok(Vec::new()));
        let a = ("missing-registration", "g", "launch");
        let b = ("ship-missing", "g", "linux");
        store
            .record_scan(&report(&[a, b], false), None, "/r", 10)
            .unwrap();
        let path = store.dir.join(IMPROVEMENTS_FILE);
        let good = std::fs::read(&path).unwrap();
        let big = vec![b' '; crate::scan::source::MAX_BYTES as usize + 1];
        for bad in [good[..good.len() / 2].to_vec(), big] {
            std::fs::write(&path, &bad).unwrap();
            assert!(store.improvements().is_err());
            assert!(
                store
                    .record_scan(&report(&[a], false), None, "/r", 20)
                    .is_err()
            );
            assert!(std::fs::read(&path).unwrap() == bad, "history overwritten");
        }
        // Not a regular file (an I/O error other than NotFound).
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(store.improvements().is_err());
        assert!(
            store
                .record_scan(&report(&[a], false), None, "/r", 20)
                .is_err()
        );
        assert!(path.is_dir());
        // Readable again: b's history (first_seen, resolution) carries on.
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, &good).unwrap();
        let merged = store
            .record_scan(&report(&[a], false), None, "/r", 30)
            .unwrap();
        let b_imp = merged
            .iter()
            .find(|i| i.id == candidate_id("/r", b.0, b.1, b.2))
            .unwrap();
        assert_eq!((b_imp.first_seen, b_imp.resolved), (10, Some(30)));
    }

    #[test]
    fn a_line_torn_inside_utf8_hides_no_later_observation() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let store = Store::new(data.path(), repo.path());
        let sub = Subject::Asset("a".into());
        store.skip(sub.clone(), "一つ目", None, 1).unwrap();
        // A crash mid-write tears a multi-byte character ("理" is 3 bytes).
        let log = store.dir.join(OBSERVATIONS_FILE);
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(b"{\"event\":\"skipped\",\"reason\":\"\xe7\x90")
            .unwrap();
        store.skip(sub.clone(), "二つ目", None, 2).unwrap();
        store.skip(sub, "三つ目", None, 3).unwrap();
        let reasons: Vec<_> = store
            .observations()
            .into_iter()
            .map(|o| match o {
                Observation::Skipped { reason, .. } => reason,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(reasons, ["一つ目", "二つ目", "三つ目"]);
    }

    /// Every file under `dir` with its bytes, sorted.
    fn tree_bytes(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if std::fs::symlink_metadata(&p).unwrap().is_dir() {
                    stack.push(p);
                } else {
                    let bytes = std::fs::read(&p).unwrap();
                    out.push((p, bytes));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn a_scan_a_link_and_a_skip_leave_the_repo_byte_identical() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let put = |rel: &str, text: &str| {
            let p = repo.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        put(
            "dev/scaffold/registry.json",
            "{\"schema\": 1, \"widgets\": {\"a\": {\"ships\": [\"macos\"]}}, \"assets\": []}\n",
        );
        put("widgets/a/Cargo.toml", "[package]\nname = \"a\"\n");
        put("widgets/a/src/main.rs", "fn main() {}\n");
        put(".git/HEAD", "ref: refs/heads/main\n");
        put(
            ".git/refs/heads/main",
            "0123456789abcdef0123456789abcdef01234567\n",
        );
        let before = tree_bytes(repo.path());

        let report = crate::scan::scan(repo.path());
        assert!(!report.findings.is_empty());
        let commit = crate::gitread::head_commit(repo.path());
        let identity = crate::gitread::repo_identity(repo.path());
        let store = Store::new(data.path(), repo.path());
        let merged = store
            .record_scan(&report, commit.clone(), &identity, 1)
            .unwrap();
        let vdir = data.path().join("noa/verifications/ver-1");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("events.jsonl"), "{}\n").unwrap();
        let sub = Subject::Improvement(merged[0].id.clone());
        store
            .link_session(sub.clone(), LinkKind::Verify, "ver-1", 2)
            .unwrap();
        store.skip(sub, "不要", commit, 3).unwrap();
        let _ = crate::registry::load(repo.path());

        assert!(tree_bytes(repo.path()) == before, "the repo changed");
        assert!(store.dir.join(SCAN_FILE).is_file());
        assert!(store.dir.join(IMPROVEMENTS_FILE).is_file());
        assert!(store.dir.join(OBSERVATIONS_FILE).is_file());
    }

    #[test]
    fn the_repo_key_is_the_name_and_a_hash_of_the_canonical_path() {
        let repo = tempfile::tempdir().unwrap();
        let key = repo_key(repo.path());
        let canonical = std::fs::canonicalize(repo.path()).unwrap();
        assert_eq!(
            key,
            format!(
                "{}-{}",
                canonical.file_name().unwrap().to_string_lossy(),
                hash::hex(canonical.as_os_str().as_encoded_bytes())
            )
        );
        let slash = PathBuf::from(format!("{}/", repo.path().display()));
        assert_eq!(repo_key(&slash), key);
    }
}
