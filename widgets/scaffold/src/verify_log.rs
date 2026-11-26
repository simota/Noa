//! What a Verify session's `events.jsonl` says, read conservatively (L2-4,
//! Decision 7). Only the `event` tag and the few fields below are read; the
//! log is never reduced the way `widgets/verify` does.
//!
//! Verify writes each line as a JSON array: one batch of events
//! (`widgets/verify/src/store.rs`, `fn line`), and reads nothing else. The
//! batches are read in order and flattened into one stream of events.

use std::io::Read as _;
use std::path::Path;

use serde_json::Value;

use crate::model::SessionFact;
use crate::scan::source::open_nofollow;

/// The fact of the session in folder `path`.
pub fn fact(path: &Path) -> SessionFact {
    let log = path.join("events.jsonl");
    match std::fs::symlink_metadata(&log) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SessionFact::Missing,
        Err(e) => return SessionFact::Unreadable(e.to_string()),
        Ok(meta) if !meta.file_type().is_file() => {
            return SessionFact::Unreadable("not a regular file".into());
        }
        Ok(_) => {}
    }
    let file = match open_nofollow(&log) {
        Ok(f) => f,
        Err(e) => return SessionFact::Unreadable(e.to_string()),
    };
    let mut text = Vec::new();
    if let Err(e) = (&file).read_to_end(&mut text) {
        return SessionFact::Unreadable(e.to_string());
    }
    let mut batches = match batches(&text) {
        Ok(b) => b.into_iter(),
        Err(e) => return SessionFact::Unreadable(e),
    };
    let mut first = batches.next().unwrap_or_default().into_iter();
    let Some(created) = first.next().filter(|v| tag(v) == Some("created")) else {
        return SessionFact::Unreadable("first event is not created".into());
    };
    let rebuild = &created["rebuild"];
    let clean = rebuild.get("worktree_patch").is_none_or(Value::is_null)
        && rebuild
            .get("untracked")
            .is_none_or(|u| u.is_null() || u.as_array().is_some_and(Vec::is_empty));
    let commit = clean
        .then(|| rebuild["head_sha"].as_str().map(str::to_owned))
        .flatten();

    let mut seen = Seen::default();
    first
        .chain(batches.flatten())
        .for_each(|event| seen.apply(&event));
    if seen.closed {
        SessionFact::Finished {
            decision: seen.pending,
            commit,
        }
    } else if seen.events > 0 {
        SessionFact::Running
    } else {
        SessionFact::Registered
    }
}

/// What the events after `created` say so far.
#[derive(Default)]
struct Seen {
    events: usize,
    pending: Option<String>,
    closed: bool,
}

impl Seen {
    fn apply(&mut self, event: &Value) {
        self.events += 1;
        match tag(event) {
            Some("round_started") => self.pending = None,
            Some("judge_accepted") => {
                if let Some(d) = event["output"]["verdict"]["decision"].as_str() {
                    self.pending = Some(d.to_owned());
                }
            }
            Some("session_closed") => self.closed = true,
            // As `widgets/verify` reduces it: more rounds reopen an ended
            // session and drop its verdict (it was for the old limit).
            Some("rounds_extended") => {
                self.closed = false;
                self.pending = None;
            }
            _ => {}
        }
    }
}

/// The log's batches, by the rule Verify reads it with
/// (`widgets/verify/src/store.rs`, `fn parse`): blank lines are skipped, and
/// a line that is not a batch is skipped only when it is cut short (the last
/// line, not ended by a newline, as a crash mid-append leaves it). Any other
/// such line is corruption: the error names it.
fn batches(text: &[u8]) -> Result<Vec<Vec<Value>>, String> {
    let mut batches = Vec::new();
    for (n, raw) in text.split_inclusive(|&b| b == b'\n').enumerate() {
        if raw.trim_ascii().is_empty() {
            continue;
        }
        match serde_json::from_slice::<Vec<Value>>(raw) {
            Ok(batch) => batches.push(batch),
            Err(_) if !raw.ends_with(b"\n") => {}
            Err(e) => return Err(format!("line {}: {e}", n + 1)),
        }
    }
    Ok(batches)
}

fn tag(v: &Value) -> Option<&str> {
    v.get("event").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Events in the shape `widgets/verify` serializes them
    // (`widgets/verify/src/model/types.rs`, `enum Event`); each log line is
    // one batch, a JSON array (`widgets/verify/src/store.rs`, `fn line`).

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn created(rebuild: &str) -> String {
        format!(
            r#"{{"event":"created","id":"ver-1","request":"r","target":{{"kind":"diff","repo":"/r","base":"main","head":{{"ref":"feat"}}}},"manifest":{{"label":"feat … main","repo":"/r","base_ref":"main","base_sha":"{SHA}","head_ref":"feat","head_sha":"{SHA}","merge_base":"{SHA}","files":[],"omitted":[],"diff_bytes":0,"diff_digest":1,"captured_at":1}},"rebuild":{rebuild},"isolation":"worktree","user_criteria":[],"workdir":"/r","main_worktree":"/r","max_rounds":2,"judge":{{"id":"judge","kind":"claude","role":"judge","pane_id":null,"setup":{{"model":"opus"}}}},"verifiers":[{{"id":"claude","kind":"claude","role":"verifier","pane_id":null,"setup":{{"model":"opus"}},"strategy":{{"strategy":"builtin","id":"conformance"}}}}],"confirm":true,"at":1}}"#
        )
    }

    fn clean() -> String {
        created(&format!(r#"{{"head_sha":"{SHA}"}}"#))
    }

    fn round(no: u32) -> String {
        format!(r#"{{"event":"round_started","no":{no},"kind":"blind","at":2}}"#)
    }

    fn judge(decision: &str) -> String {
        format!(
            r#"{{"event":"judge_accepted","round":1,"output":{{"summary":"s","rulings":{{}},"defect_ops":[],"assign":{{}},"verdict":{{"decision":"{decision}","rationale":"r","blocking":[]}},"close":true}}}}"#
        )
    }

    fn judge_without_verdict() -> String {
        r#"{"event":"judge_accepted","round":1,"output":{"summary":"s","rulings":{},"defect_ops":[],"assign":{},"close":false}}"#.into()
    }

    fn closed() -> String {
        r#"{"event":"session_closed"}"#.into()
    }

    fn extended() -> String {
        r#"{"event":"rounds_extended","max_rounds":5,"at":9}"#.into()
    }

    /// One log line: a batch of `events`.
    fn line(events: &[String]) -> String {
        format!("[{}]", events.join(","))
    }

    /// A session whose log is `lines`, each written as given.
    fn raw(lines: &[String]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ver-1");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
        (dir, path)
    }

    /// A session whose log is exactly `text`.
    fn text(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ver-1");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("events.jsonl"), text).unwrap();
        (dir, path)
    }

    /// A session whose log holds `batches`, one line each.
    fn session(batches: &[Vec<String>]) -> (tempfile::TempDir, std::path::PathBuf) {
        raw(&batches.iter().map(|b| line(b)).collect::<Vec<_>>())
    }

    fn finished(decision: Option<&str>, commit: Option<&str>) -> SessionFact {
        SessionFact::Finished {
            decision: decision.map(str::to_owned),
            commit: commit.map(str::to_owned),
        }
    }

    #[test]
    fn missing_and_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(fact(&dir.path().join("ver-x")), SessionFact::Missing);
        for log in [
            vec![line(&[round(1)])],
            vec!["not json".into()],
            vec!["[]".into()],
            // Verify never writes a bare event object, only batches.
            vec![clean()],
            vec![],
        ] {
            let (_d, p) = raw(&log);
            assert!(matches!(fact(&p), SessionFact::Unreadable(_)), "{log:?}");
        }
    }

    #[test]
    fn registered_running_finished() {
        let (_d, p) = session(&[vec![clean()]]);
        assert_eq!(fact(&p), SessionFact::Registered);
        let (_d, p) = session(&[vec![clean()], vec![round(1)], vec![judge("pass")]]);
        assert_eq!(fact(&p), SessionFact::Running);
        let (_d, p) = session(&[
            vec![clean()],
            vec![round(1)],
            vec![judge("pass")],
            vec![closed()],
        ]);
        assert_eq!(fact(&p), finished(Some("pass"), Some(SHA)));
    }

    #[test]
    fn a_line_holding_several_events_is_read_in_order() {
        // As Verify writes the end of a session: the ruling and the close in
        // one batch, beside other events.
        let (_d, p) = session(&[
            vec![clean()],
            vec![round(1)],
            vec![
                r#"{"event":"reply_accepted","round":1,"to":"judge","at":3}"#.into(),
                judge("pass_with_issues"),
                r#"{"event":"phase_changed","round":1,"phase":"done"}"#.into(),
                closed(),
            ],
        ]);
        assert_eq!(fact(&p), finished(Some("pass_with_issues"), Some(SHA)));
        // The order inside a line counts: a round after the ruling clears it.
        let (_d, p) = session(&[
            vec![clean()],
            vec![round(1), judge("pass"), round(2)],
            vec![closed()],
        ]);
        assert_eq!(fact(&p), finished(None, Some(SHA)));
        // The first line's events after `created` count too.
        let (_d, p) = session(&[vec![clean(), round(1)]]);
        assert_eq!(fact(&p), SessionFact::Running);
    }

    #[test]
    fn a_new_round_clears_the_decision() {
        let (_d, p) = session(&[
            vec![clean()],
            vec![round(1)],
            vec![judge("pass")],
            vec![round(2)],
            vec![judge_without_verdict()],
            vec![closed()],
        ]);
        assert_eq!(fact(&p), finished(None, Some(SHA)));
    }

    #[test]
    fn uncommitted_changes_have_no_commit() {
        for rebuild in [
            format!(r#"{{"head_sha":"{SHA}","worktree_patch":42}}"#),
            format!(r#"{{"head_sha":"{SHA}","untracked":[["a.txt",1]]}}"#),
        ] {
            let (_d, p) = session(&[vec![created(&rebuild)], vec![judge("pass"), closed()]]);
            assert_eq!(fact(&p), finished(Some("pass"), None));
        }
        let (_d, p) = session(&[
            vec![created(&format!(
                r#"{{"head_sha":"{SHA}","untracked":[]}}"#
            ))],
            vec![closed()],
        ]);
        assert_eq!(fact(&p), finished(None, Some(SHA)));
    }

    #[test]
    fn extending_rounds_reopens_a_closed_session() {
        let ended = || vec![vec![clean()], vec![round(1)], vec![judge("pass"), closed()]];
        // Reopened: running again, never Finished with the old verdict.
        let mut log = ended();
        log.push(vec![extended()]);
        let (_d, p) = session(&log);
        assert_eq!(fact(&p), SessionFact::Running);
        log.push(vec![round(2)]);
        let (_d, p) = session(&log);
        assert_eq!(fact(&p), SessionFact::Running);
        // Closed again: only the new round's verdict counts.
        log.push(vec![judge("fail"), closed()]);
        let (_d, p) = session(&log);
        assert_eq!(fact(&p), finished(Some("fail"), Some(SHA)));
        // Closed again with no new verdict: no decision, not the old pass.
        let mut log = ended();
        log.push(vec![extended()]);
        log.push(vec![closed()]);
        let (_d, p) = session(&log);
        assert_eq!(fact(&p), finished(None, Some(SHA)));
    }

    #[test]
    fn a_torn_last_line_is_skipped() {
        // Cut short by a crash mid-append: no newline after it.
        let log = |tail: &str| format!("{}\n{}\n{tail}", line(&[clean()]), line(&[judge("fail")]));
        let (_d, p) = text(&log(r#"[{"event":"sess"#));
        assert_eq!(fact(&p), SessionFact::Running);
        // A whole batch without its newline still counts, as in Verify.
        let (_d, p) = text(&log(&line(&[closed()])));
        assert_eq!(fact(&p), finished(Some("fail"), Some(SHA)));
    }

    #[test]
    fn a_whole_line_that_is_not_a_batch_is_corruption() {
        // Verify refuses such a log (`widgets/verify/src/store.rs`,
        // `fn parse`), so it is never read as 確認済み.
        let pass = || vec![line(&[clean()]), line(&[round(1)]), line(&[judge("pass")])];
        for bad in [
            "not json",
            r#"[{"event":"sess"#,
            r#"{"event":"session_closed"}"#,
        ] {
            // In the middle of the log.
            let mut log = pass();
            log.push(bad.into());
            log.push(line(&[closed()]));
            let (_d, p) = raw(&log);
            assert!(matches!(fact(&p), SessionFact::Unreadable(_)), "{bad}");
            // Last, but ended by its newline: not torn.
            let mut log = pass();
            log.push(line(&[closed()]));
            log.push(bad.into());
            let (_d, p) = raw(&log);
            assert!(matches!(fact(&p), SessionFact::Unreadable(_)), "{bad}");
        }
    }

    #[test]
    fn blank_lines_are_skipped() {
        let (_d, p) = raw(&[
            String::new(),
            line(&[clean()]),
            "  ".into(),
            line(&[judge("pass"), closed()]),
        ]);
        assert_eq!(fact(&p), finished(Some("pass"), Some(SHA)));
    }

    /// A log written by `widgets/verify` itself: its demo session, stored
    /// through its `Store` (30 batches, two rounds, a `fail` verdict).
    const VERIFY_DEMO: &str = include_str!("testdata/verify-demo.events.jsonl");

    #[test]
    fn a_log_written_by_verify_is_read() {
        let lines: Vec<String> = VERIFY_DEMO.lines().map(str::to_owned).collect();
        assert_eq!(lines.len(), 30);
        let head = "a71be09d2c5f4e18b3c9d0e7f6a5b4c3d2e1f009";
        let (_d, p) = raw(&lines);
        assert_eq!(fact(&p), finished(Some("fail"), Some(head)));
        let (_d, p) = raw(&lines[..1]);
        assert_eq!(fact(&p), SessionFact::Registered);
        let (_d, p) = raw(&lines[..lines.len() - 1]);
        assert_eq!(fact(&p), SessionFact::Running);
    }
}
