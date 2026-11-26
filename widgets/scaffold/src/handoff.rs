//! Hand-off of a candidate to an agent: a local task file in the format of
//! `skills/noa-tasks/SKILL.md` (I6), which the Tasks widget lists, then a
//! `linked … kind: task` observation (Decision 8).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::model::{Improvement, Subject, rule_label};
use crate::store::{Event, LinkKind, Store, temp_sibling};
use crate::time::Millis;

/// The asset every registration candidate comes from.
pub const CHECKER_ASSET: &str = "check-widget-registration";
pub const LABEL: &str = "scaffold";

/// A linked Verify or Review session named in the brief.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Related {
    pub kind: LinkKind,
    pub id: String,
    pub path: PathBuf,
}

/// A header value on one line: control characters become spaces.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

pub fn title(imp: &Improvement) -> String {
    one_line(&format!(
        "[足場] {} {}: {}",
        imp.widget,
        imp.target,
        rule_label(&imp.rule)
    ))
}

/// The work brief, in Markdown (I6).
pub fn brief(imp: &Improvement, related: &[Related]) -> String {
    let mut s = String::new();
    s.push_str("## 改善候補\n\n");
    s.push_str(&format!("- id: {}\n", imp.id));
    s.push_str(&format!(
        "- rule: {}（{}）\n",
        imp.rule,
        rule_label(&imp.rule)
    ));
    s.push_str(&format!("- target: {}/{}\n", imp.widget, imp.target));
    s.push_str("\n## 根拠\n\n");
    for e in &imp.evidence {
        s.push_str(&format!("- {}:{}\n", e.path, e.line));
    }
    if imp.evidence.is_empty() {
        s.push_str("- なし\n");
    }
    s.push_str("\n## 関連\n\n");
    for r in related {
        s.push_str(&format!(
            "- {} {}: {}\n",
            r.kind.label(),
            r.id,
            r.path.display()
        ));
    }
    if related.is_empty() {
        s.push_str("- なし\n");
    }
    s.push_str("\n## 完了の条件\n\n");
    s.push_str(&format!(
        "- `python3 -I -B scripts/check-widget-registration.py` が `{} {}: {}` を報告しなくなること。\n",
        imp.widget, imp.target, imp.rule
    ));
    s.push_str("- 手順は `docs/runbooks/add-widget.md` に従う。\n");
    s.push_str("\n## 報告\n\n");
    s.push_str("- Tasks の実行指示のとおり、結果を `$NOA_RESULT` に書く。\n");
    s
}

/// The whole task file: header, then the brief.
pub fn task_text(imp: &Improvement, related: &[Related]) -> String {
    format!(
        "---\ntitle: {}\nstate: open\nlabels: {LABEL}\nscaffold-asset: {CHECKER_ASSET}\nscaffold-improvement: {}\n---\n{}",
        title(imp),
        one_line(&imp.id),
        brief(imp, related)
    )
}

fn real_dir(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_dir() => Ok(()),
        Ok(_) => Err(std::io::Error::other(format!(
            "{} がフォルダではありません（リンクには書きません）",
            path.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(path),
        Err(e) => Err(e),
    }
}

/// Writes `text` as a new `<repo>/.noa/tasks/t-<now>[-n].md` (I6): makes
/// `.noa/tasks` and `.noa/.gitignore` (`*`) when missing, refuses a `.noa`
/// or `.noa/tasks` that is a link, and never replaces a task. Gives the id
/// and the path.
pub fn create_task(repo: &Path, text: &str, now: Millis) -> std::io::Result<(String, PathBuf)> {
    let noa = repo.join(".noa");
    real_dir(&noa)?;
    let ignore = noa.join(".gitignore");
    if std::fs::symlink_metadata(&ignore).is_err() {
        std::fs::write(&ignore, "*\n")?;
    }
    let dir = noa.join("tasks");
    real_dir(&dir)?;
    let base = format!("t-{now}");
    let tmp = temp_sibling(&dir.join(format!("{base}.md")));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    let written = f.write_all(text.as_bytes()).and_then(|_| f.sync_all());
    drop(f);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let mut n = 1;
    let result = loop {
        let id = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let path = dir.join(format!("{id}.md"));
        match std::fs::hard_link(&tmp, &path) {
            Ok(()) => break Ok((id, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
            Err(e) => break Err(e),
        }
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

/// タスクにする: writes the task and records the link. Gives the task id.
pub fn hand_off(
    store: &Store,
    repo: &Path,
    imp: &Improvement,
    related: &[Related],
    now: Millis,
) -> Result<String, String> {
    let (id, path) = create_task(repo, &task_text(imp, related), now).map_err(|e| e.to_string())?;
    store
        .append(&Event::Linked {
            subject: Subject::Improvement(imp.id.clone()),
            kind: LinkKind::Task,
            r#ref: id.clone(),
            path,
            at: now,
        })
        .map_err(|e| format!("タスク {id} は作りましたが、関連付けを記録できませんでした: {e}"))?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ImprovementStatus, improvement_status};
    use crate::scan::Evidence;

    fn imp() -> Improvement {
        Improvement {
            id: "imp-0123456789abcdef".into(),
            rule: "missing-registration".into(),
            widget: "gamma".into(),
            target: "launch".into(),
            evidence: vec![
                Evidence::new("crates/noa-app/src/helper_program.rs", 83),
                Evidence::new("widgets/gamma/Cargo.toml", 2),
            ],
            first_seen: 1_732_600_000_000,
            last_seen: 1_732_600_000_000,
            resolved: None,
        }
    }

    fn related() -> Vec<Related> {
        vec![Related {
            kind: LinkKind::Verify,
            id: "ver-1732600000000".into(),
            path: PathBuf::from("/data/noa/verifications/ver-1732600000000"),
        }]
    }

    #[test]
    fn the_task_matches_the_golden_file() {
        let golden = include_str!("testdata/handoff-task.md");
        assert_eq!(task_text(&imp(), &related()), golden);
    }

    /// The format rules of `skills/noa-tasks/SKILL.md`: two `---` fences,
    /// `key: value` header lines, `title` present, `state` open or closed,
    /// `labels` comma-separated.
    fn check_skill_format(text: &str) -> Vec<(String, String)> {
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("---"));
        let mut header = Vec::new();
        for line in lines.by_ref() {
            if line == "---" {
                break;
            }
            let (k, v) = line.split_once(':').expect("a key: value line");
            assert!(!k.trim().is_empty() && !k.contains(' '), "{line}");
            assert!(v.starts_with(' '), "{line}");
            header.push((k.trim().to_owned(), v.trim().to_owned()));
        }
        assert!(lines.next().is_some(), "a body after the header");
        let get = |k: &str| {
            header
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        assert!(get("title").is_some_and(|t| !t.is_empty()));
        assert!(matches!(get("state").as_deref(), Some("open" | "closed")));
        assert!(get("labels").is_some_and(|l| l.split(',').all(|x| !x.trim().is_empty())));
        assert!(!text.contains("TODO") && !text.contains("FIXME"));
        header
    }

    #[test]
    fn create_task_writes_a_skill_conformant_file_and_links_it() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path(), repo.path());
        let id = hand_off(&store, repo.path(), &imp(), &[], 1_732_600_000_000).unwrap();
        assert_eq!(id, "t-1732600000000");
        let path = repo.path().join(".noa/tasks/t-1732600000000.md");
        let header = check_skill_format(&std::fs::read_to_string(&path).unwrap());
        assert!(header.contains(&("scaffold-asset".into(), CHECKER_ASSET.into())));
        assert!(header.contains(&("scaffold-improvement".into(), "imp-0123456789abcdef".into())));
        assert_eq!(
            std::fs::read_to_string(repo.path().join(".noa/.gitignore")).unwrap(),
            "*\n"
        );
        // The same millisecond gives -2; nothing is replaced, no temp file left.
        let again = hand_off(&store, repo.path(), &imp(), &[], 1_732_600_000_000).unwrap();
        assert_eq!(again, "t-1732600000000-2");
        let mut names: Vec<_> = std::fs::read_dir(repo.path().join(".noa/tasks"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["t-1732600000000-2.md", "t-1732600000000.md"]);
        assert_eq!(
            improvement_status(&imp(), &store.observations()),
            ImprovementStatus::HandedOff("t-1732600000000-2".into())
        );
    }

    #[test]
    fn a_linked_noa_folder_is_refused() {
        let repo = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), repo.path().join(".noa")).unwrap();
        assert!(create_task(repo.path(), "x", 1).is_err());
        std::fs::remove_file(repo.path().join(".noa")).unwrap();
        std::fs::create_dir(repo.path().join(".noa")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), repo.path().join(".noa/tasks")).unwrap();
        assert!(create_task(repo.path(), "x", 1).is_err());
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[test]
    fn header_values_stay_on_one_line() {
        let mut i = imp();
        i.widget = "evil\ntitle: x".into();
        let text = task_text(&i, &[]);
        let header: Vec<&str> = text.lines().skip(1).take_while(|l| *l != "---").collect();
        assert_eq!(header.len(), 5, "{header:?}");
    }

    #[test]
    fn linking_a_session_stores_only_its_reference() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path(), repo.path());
        for (folder, id, kind) in [
            ("verifications", "ver-1", LinkKind::Verify),
            ("reviews", "rev-1", LinkKind::Review),
        ] {
            let dir = data.path().join("noa").join(folder).join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("events.jsonl"), "{\"event\":\"created\"}\n").unwrap();
            std::fs::write(dir.join("report.md"), "secret content").unwrap();
            store
                .link_session(Subject::Asset("a".into()), kind, id, 1)
                .unwrap();
        }
        let mut files = Vec::new();
        walk(data.path(), &mut files);
        files.sort();
        let rel: Vec<String> = files
            .iter()
            .map(|p| p.strip_prefix(data.path()).unwrap().display().to_string())
            .collect();
        let key = crate::store::repo_key(repo.path());
        assert_eq!(
            rel,
            [
                "noa/reviews/rev-1/events.jsonl".to_owned(),
                "noa/reviews/rev-1/report.md".to_owned(),
                format!("noa/scaffold/{key}/observations.jsonl"),
                "noa/verifications/ver-1/events.jsonl".to_owned(),
                "noa/verifications/ver-1/report.md".to_owned(),
            ]
        );
        let log = std::fs::read_to_string(store.dir.join("observations.jsonl")).unwrap();
        assert!(!log.contains("secret"));
        assert_eq!(std::fs::read_dir(repo.path()).unwrap().count(), 0);
    }

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}
