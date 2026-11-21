//! `Noa +ctl`'s journal (AU-04): one append-only JSONL file per
//! orchestrator pane (`noa_config::ctl_journal_path`), so an orchestrator
//! whose own context was cut can recover what it spawned, sent, saw and
//! answered (`status`, `status --answers`). The client writes it; the
//! server keeps no such record.
//!
//! Every line is one event with `"ev"` and `"ts"` (unix seconds): `spawn`,
//! `send`, `outcome` and `close`. A journal that cannot be written never
//! fails a command: the first failure is a warning on stderr, the rest are
//! silent.
//!
//! The journal holds sent tasks and answered prompts, so it is made `0600`
//! in a `0700` directory. Commands run at once (background `send --wait`s):
//! each append and each rotation holds an exclusive `flock` on
//! `<journal>.lock`, so a rotation never renames its halved copy over a
//! line appended meanwhile.

use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// A journal over this size when it is opened loses its oldest half.
pub(crate) const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;

/// The most characters of a sent text the journal keeps.
pub(crate) const TEXT_CHARS: usize = 200;

pub(crate) struct Journal {
    /// `None`: this process has no pane of its own, so nothing is kept.
    path: Option<PathBuf>,
    warned: bool,
}

/// Unix seconds now.
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl Journal {
    /// The journal of the pane this process runs in (`NOA_SURFACE_ID`).
    pub(crate) fn open() -> Self {
        let path = std::env::var(super::SURFACE_ID_ENV)
            .ok()
            .and_then(|surface_id| noa_config::ctl_journal_path(&surface_id));
        match path {
            Some(path) => Self::at(path),
            None => Self {
                path: None,
                warned: false,
            },
        }
    }

    /// The journal at `path`, its oldest half dropped when it is over
    /// [`MAX_JOURNAL_BYTES`].
    pub(crate) fn at(path: PathBuf) -> Self {
        let mut journal = Self {
            path: Some(path),
            warned: false,
        };
        if let Some(path) = journal.path.clone()
            && std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_JOURNAL_BYTES)
            && let Err(err) = lock(&path).and_then(|_lock| {
                // Another command may have rotated it while this one waited.
                if std::fs::metadata(&path)?.len() > MAX_JOURNAL_BYTES {
                    drop_oldest_half(&path)?;
                }
                Ok(())
            })
        {
            journal.warn(&path, &err);
        }
        journal
    }

    fn warn(&mut self, path: &Path, err: &std::io::Error) {
        if !self.warned {
            self.warned = true;
            eprintln!(
                "Noa +ctl: warning: cannot write the journal {}: {err}; carrying on without it",
                path.display()
            );
        }
    }

    /// Append the event `ev` with `fields` (an object), stamped with `ts`.
    /// One write per line, through `O_APPEND`, so the lines of commands
    /// running at once (background `send --wait`s) do not interleave, under
    /// the journal's lock, so a rotation does not drop it.
    pub(crate) fn append(&mut self, ev: &str, fields: serde_json::Value) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let mut event = serde_json::json!({ "ev": ev, "ts": now_secs() });
        if let (Some(event), serde_json::Value::Object(fields)) = (event.as_object_mut(), fields) {
            event.extend(fields);
        }
        let mut line = event.to_string();
        line.push('\n');
        let written = path
            .parent()
            .map_or(Ok(()), |dir| {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)
            })
            .and_then(|()| lock(&path))
            .and_then(|_lock| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .open(&path)?
                    .write_all(line.as_bytes())
            });
        if let Err(err) = written {
            self.warn(&path, &err);
        }
    }

    /// Every event, oldest first; a line that does not parse is skipped.
    pub(crate) fn events(&self) -> Vec<serde_json::Value> {
        let Some(text) = self
            .path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
        else {
            return Vec::new();
        };
        parse_events(&text)
    }
}

pub(crate) fn parse_events(text: &str) -> Vec<serde_json::Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event.get("ev").is_some_and(serde_json::Value::is_string))
        .collect()
}

/// An exclusive `flock` on `<path>.lock` (made `0600`), held until the
/// returned file is dropped. A lock file beside the journal, not the
/// journal itself: a rotation replaces the journal's inode.
fn lock(path: &Path) -> std::io::Result<std::fs::File> {
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)?;
    loop {
        // SAFETY: `file` owns the descriptor for the whole call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(file);
        }
        let err = std::io::Error::last_os_error();
        // A signal the wait catches (SIGINT, SIGTERM) can interrupt it.
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Keep the newer half of the file at `path`, cut at a line start, swapped
/// in by a rename so a reader never sees half a file. The caller holds the
/// journal's [`lock`].
fn drop_oldest_half(path: &Path) -> std::io::Result<()> {
    let bytes = std::fs::read(path)?;
    let middle = bytes.len() / 2;
    let start = bytes[middle..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(bytes.len(), |at| middle + at + 1);
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?
        .write_all(&bytes[start..])?;
    std::fs::rename(&temp, path)
}

/// The first [`TEXT_CHARS`] characters of a sent text.
pub(crate) fn clip(text: &str) -> String {
    text.chars().take(TEXT_CHARS).collect()
}

/// What the journal knows of one worker.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Worker {
    pub(crate) pane: u64,
    pub(crate) label: Option<String>,
    pub(crate) argv: Option<serde_json::Value>,
    pub(crate) tier: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) worktree: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) result: Option<String>,
    /// The last text sent that was not an answer.
    pub(crate) last_task: Option<String>,
    /// The report count before the last send.
    pub(crate) last_sent_seq: Option<u64>,
    /// `{reason, seq, ts}` of the last wait outcome.
    pub(crate) last_outcome: Option<serde_json::Value>,
    pub(crate) answers: usize,
    pub(crate) closed: bool,
}

fn pane_of(event: &serde_json::Value) -> Option<u64> {
    event.get("pane")?.as_u64()
}

/// A pane id as the wire writes it (`"9"`).
fn pane_field(value: &serde_json::Value) -> Option<u64> {
    value.as_str()?.parse().ok()
}

fn text_field(event: &serde_json::Value, key: &str) -> Option<String> {
    event.get(key)?.as_str().map(str::to_string)
}

/// The workers the events name, in the order they were first spawned. Only
/// a `spawn` (a recovered one is journaled as one too) makes a worker;
/// another event attaches to the worker spawned under its pane, and one for
/// any other pane (`send --any-pane`, `wait` on this pane, a mistyped
/// `close`) names no worker.
pub(crate) fn workers(events: &[serde_json::Value]) -> Vec<Worker> {
    let mut order: Vec<u64> = Vec::new();
    let mut known: HashMap<u64, Worker> = HashMap::new();
    for event in events {
        let Some(pane) = pane_of(event) else {
            continue;
        };
        if event["ev"] == "spawn" {
            known.entry(pane).or_insert_with(|| {
                order.push(pane);
                Worker::default()
            });
        }
        let Some(worker) = known.get_mut(&pane) else {
            moved_results(&mut known, event);
            continue;
        };
        match event["ev"].as_str() {
            Some("spawn") => {
                // A pane id given out again (after Noa restarted) is a new
                // worker: forget the old one's record.
                *worker = Worker {
                    pane,
                    label: text_field(event, "label"),
                    argv: event.get("argv").cloned(),
                    tier: text_field(event, "tier"),
                    cwd: text_field(event, "cwd"),
                    worktree: text_field(event, "worktree"),
                    branch: text_field(event, "branch"),
                    result: text_field(event, "result"),
                    ..Worker::default()
                };
            }
            Some("send") => {
                if event["answer"].as_bool() == Some(true) {
                    worker.answers += 1;
                } else {
                    worker.last_task = text_field(event, "text");
                }
                worker.last_sent_seq = event["seqBefore"].as_u64();
            }
            Some("outcome") => {
                worker.last_outcome = Some(serde_json::json!({
                    "reason": event["reason"],
                    "seq": event["seq"],
                    "ts": event["ts"],
                }));
                if worker.label.is_none() {
                    worker.label = text_field(event, "label");
                }
            }
            Some("close") => {
                worker.closed = true;
                // `close --prune` moved the result out of the worktree.
                if let Some(result) = event["worktree"]["result"].as_str() {
                    worker.result = Some(result.to_string());
                }
            }
            _ => {}
        }
        moved_results(&mut known, event);
    }
    order
        .into_iter()
        .filter_map(|pane| known.remove(&pane))
        .collect()
}

/// A `close --prune`'s moves of other workers' results out of the worktree
/// it removed (`worktree.moved`).
fn moved_results(known: &mut HashMap<u64, Worker>, event: &serde_json::Value) {
    for moved in event["worktree"]["moved"].as_array().into_iter().flatten() {
        if let (Some(pane), Some(to)) = (pane_field(&moved["paneId"]), moved["to"].as_str())
            && let Some(worker) = known.get_mut(&pane)
        {
            worker.result = Some(to.to_string());
        }
    }
}

/// When the last `send` to `pane` with report count `seq` before it was
/// made (its `ts`), for a wait that resumes it (`wait --after PANE=SEQ`).
pub(crate) fn sent_at(events: &[serde_json::Value], pane: u64, seq: u64) -> Option<u64> {
    events
        .iter()
        .rev()
        .find(|event| {
            event["ev"] == "send"
                && pane_of(event) == Some(pane)
                && event["seqBefore"].as_u64() == Some(seq)
        })?
        .get("ts")?
        .as_u64()
}

/// `status --answers`: every answered prompt, oldest first, with the
/// worker's label when the journal knows it.
pub(crate) fn answers(events: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut labels: HashMap<u64, String> = HashMap::new();
    let mut lines = Vec::new();
    for event in events {
        let Some(pane) = pane_of(event) else {
            continue;
        };
        match event["ev"].as_str() {
            Some("spawn") => match text_field(event, "label") {
                Some(label) => {
                    labels.insert(pane, label);
                }
                None => {
                    labels.remove(&pane);
                }
            },
            Some("send") if event["answer"].as_bool() == Some(true) => {
                let mut line = serde_json::json!({
                    "ts": event["ts"],
                    "paneId": noa_ipc::WireId(pane),
                    "text": event["text"],
                    "screen": event.get("screen").cloned().unwrap_or_default(),
                });
                if let Some(label) = labels.get(&pane) {
                    line["label"] = label.clone().into();
                }
                lines.push(line);
            }
            _ => {}
        }
    }
    lines
}

impl Worker {
    /// The journal's fields of a `status` line, each only when known.
    pub(crate) fn add_to(&self, line: &mut serde_json::Value) {
        let mut put = |key: &str, value: Option<serde_json::Value>| {
            if let Some(value) = value {
                line[key] = value;
            }
        };
        put("tier", self.tier.clone().map(Into::into));
        put("argv", self.argv.clone());
        put("lastTask", self.last_task.clone().map(Into::into));
        put("lastSentSeq", self.last_sent_seq.map(Into::into));
        put("lastOutcome", self.last_outcome.clone());
        put("answers", Some(self.answers.into()));
        put("result", self.result.clone().map(Into::into));
        put("worktree", self.worktree.clone().map(Into::into));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noa-journal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("ctl").join("x.jsonl")
    }

    #[test]
    fn events_are_appended_one_line_each_with_ev_and_ts() {
        let path = temp_path("append");
        let mut journal = Journal::at(path.clone());
        journal.append("spawn", serde_json::json!({ "pane": 4, "label": "fix" }));
        journal.append("close", serde_json::json!({ "pane": 4, "worktree": null }));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        let events = journal.events();
        assert_eq!(events[0]["ev"], "spawn");
        assert_eq!(events[0]["label"], "fix");
        assert!(events[0]["ts"].as_u64().unwrap() > 1_700_000_000);
        assert_eq!(events[1]["ev"], "close");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_journal_that_cannot_be_written_warns_and_does_not_fail() {
        // A directory where the file would go: every write fails.
        let path = temp_path("unwritable");
        std::fs::create_dir_all(&path).unwrap();
        let mut journal = Journal::at(path.clone());
        journal.append("spawn", serde_json::json!({ "pane": 1 }));
        assert!(journal.warned);
        journal.append("spawn", serde_json::json!({ "pane": 2 }));
        assert!(journal.events().is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());

        let mut none = Journal {
            path: None,
            warned: false,
        };
        none.append("spawn", serde_json::json!({ "pane": 1 }));
        assert!(!none.warned);
    }

    #[test]
    fn a_journal_over_its_bound_keeps_its_newer_half_whole_lines_only() {
        let path = temp_path("bound");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let line = format!(
            "{{\"ev\":\"send\",\"pane\":1,\"text\":\"{}\"}}\n",
            "x".repeat(99)
        );
        let count = (MAX_JOURNAL_BYTES as usize / line.len()) + 10;
        let numbered: String = (0..count)
            .map(|n| line.replace("\"pane\":1", &format!("\"pane\":{n}")))
            .collect();
        std::fs::write(&path, &numbered).unwrap();
        let journal = Journal::at(path.clone());
        let events = journal.events();
        let kept = std::fs::read_to_string(&path).unwrap();
        let half = numbered.len() / 2;
        assert!(kept.len() <= half && kept.len() + line.len() * 2 > half);
        assert_eq!(kept.lines().count(), events.len());
        assert_eq!(events.last().unwrap()["pane"], (count - 1) as u64);
        assert!(events[0]["pane"].as_u64().unwrap() > 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    // Review finding #11: the journal (sent tasks, answered prompts) was
    // made with the umask's permissions, readable by other local users.
    #[test]
    fn a_journal_is_private_to_its_user() {
        let path = temp_path("mode");
        let mut journal = Journal::at(path.clone());
        journal.append("spawn", serde_json::json!({ "pane": 1 }));
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);

        // A rotated journal stays private.
        let line = format!(
            "{{\"ev\":\"send\",\"pane\":1,\"text\":\"{}\"}}\n",
            "x".repeat(99)
        );
        let big = line.repeat(MAX_JOURNAL_BYTES as usize / line.len() + 10);
        std::fs::write(&path, big).unwrap();
        let journal = Journal::at(path.clone());
        assert!(std::fs::metadata(&path).unwrap().len() <= MAX_JOURNAL_BYTES);
        assert_eq!(mode(&path), 0o600);
        assert!(!journal.warned);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    // Review finding #10: a rotation read the journal, another command
    // appended, and the rotation renamed its halved copy over that line.
    // An append and a rotation each wait for the other's lock.
    #[test]
    fn appends_and_rotations_wait_for_the_journals_lock() {
        let path = temp_path("lock");
        let mut journal = Journal::at(path.clone());
        journal.append("spawn", serde_json::json!({ "pane": 1 }));
        let lines = || std::fs::read_to_string(&path).unwrap().lines().count();

        let held = lock(&path).unwrap();
        let appender = {
            let path = path.clone();
            std::thread::spawn(move || {
                Journal::at(path).append("send", serde_json::json!({ "pane": 1 }));
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(lines(), 1, "appended while another held the lock");
        drop(held);
        appender.join().unwrap();
        assert_eq!(lines(), 2);

        let line = format!(
            "{{\"ev\":\"send\",\"pane\":1,\"text\":\"{}\"}}\n",
            "x".repeat(99)
        );
        let big = line.repeat(MAX_JOURNAL_BYTES as usize / line.len() + 10);
        std::fs::write(&path, &big).unwrap();
        let held = lock(&path).unwrap();
        let rotator = {
            let path = path.clone();
            std::thread::spawn(move || drop(Journal::at(path)))
        };
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            big.len() as u64,
            "rotated while another held the lock"
        );
        drop(held);
        rotator.join().unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() <= MAX_JOURNAL_BYTES);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    fn sample() -> Vec<serde_json::Value> {
        parse_events(concat!(
            r#"{"ev":"spawn","ts":1,"pane":4,"label":"fix","argv":["claude"],"tier":"light","cwd":"/r-wt/fix","worktree":"/r-wt/fix","branch":"orch/fix","result":"/r-wt/fix/.noa/result-fix.md"}"#,
            "\n",
            r#"{"ev":"send","ts":2,"pane":4,"seqBefore":0,"text":"do it","bytes":5,"enter":true,"answer":false,"queue":false}"#,
            "\n",
            "not json\n",
            r#"{"ev":"outcome","ts":3,"pane":4,"reason":"permission","seq":2,"label":"fix"}"#,
            "\n",
            r#"{"ev":"send","ts":4,"pane":4,"seqBefore":2,"text":"1","bytes":1,"enter":true,"answer":true,"queue":false,"screen":"Allow edit?\n1. Yes"}"#,
            "\n",
            r#"{"ev":"spawn","ts":5,"pane":6,"argv":["codex"],"cwd":"/r"}"#,
            "\n",
            r#"{"ev":"close","ts":6,"pane":6,"worktree":null}"#,
            "\n",
        ))
    }

    #[test]
    fn workers_fold_each_panes_events_in_spawn_order() {
        let workers = workers(&sample());
        assert_eq!(workers.len(), 2);
        let fix = &workers[0];
        assert_eq!(fix.pane, 4);
        assert_eq!(fix.tier.as_deref(), Some("light"));
        assert_eq!(fix.branch.as_deref(), Some("orch/fix"));
        assert_eq!(fix.last_task.as_deref(), Some("do it"));
        assert_eq!(fix.last_sent_seq, Some(2));
        assert_eq!(fix.answers, 1);
        assert_eq!(fix.last_outcome.as_ref().unwrap()["reason"], "permission");
        assert!(!fix.closed);
        assert!(workers[1].closed);

        let mut line = serde_json::json!({});
        fix.add_to(&mut line);
        assert_eq!(line["lastTask"], "do it");
        assert_eq!(line["answers"], 1);
        assert_eq!(line["result"], "/r-wt/fix/.noa/result-fix.md");
        assert!(line.get("lastOutcome").is_some());

        // A spawn under a pane id seen before starts a new record.
        let mut events = sample();
        events.extend(parse_events(
            r#"{"ev":"spawn","ts":9,"pane":4,"label":"again","argv":["agy"]}"#,
        ));
        let again = &super::workers(&events)[0];
        assert_eq!(again.label.as_deref(), Some("again"));
        assert_eq!(again.answers, 0);
        assert_eq!(again.last_task, None);
    }

    // Review finding #2: `close --prune` moves the result into the main
    // worktree; the close event's `worktree.result` is where it now is.
    #[test]
    fn a_pruned_workers_result_is_where_the_close_moved_it() {
        let mut events = sample();
        events.extend(parse_events(concat!(
            r#"{"ev":"close","ts":7,"pane":4,"worktree":{"path":"/r-wt/fix","removed":true,"branchDeleted":true,"result":"/r/.noa/result-fix.md"}}"#,
            "\n",
            r#"{"ev":"spawn","ts":8,"pane":9,"label":"y"}"#,
            "\n",
            r#"{"ev":"close","ts":8,"pane":9,"worktree":{"removed":false,"kept":"x","result":"/r/.noa/result-y.md"}}"#,
            "\n",
        )));
        let workers = workers(&events);
        assert!(workers[0].closed);
        assert_eq!(workers[0].result.as_deref(), Some("/r/.noa/result-fix.md"));
        assert_eq!(workers[1].result, None);
        assert_eq!(workers[2].pane, 9);
        assert_eq!(workers[2].result.as_deref(), Some("/r/.noa/result-y.md"));
    }

    // Review finding: any event with a pane made a worker, so a pane sent
    // to with `--any-pane`, this pane after `wait <own pane>`, and a
    // mistyped `close 9999` were listed by `status` as gone workers.
    #[test]
    fn only_a_spawn_makes_a_worker() {
        let mut events = sample();
        events.extend(parse_events(concat!(
            r#"{"ev":"send","ts":7,"pane":12,"seqBefore":0,"text":"hi","answer":false}"#,
            "\n",
            r#"{"ev":"outcome","ts":8,"pane":1,"reason":"finished","seq":3}"#,
            "\n",
            r#"{"ev":"close","ts":9,"pane":9999,"worktree":null}"#,
            "\n",
        )));
        let panes: Vec<u64> = workers(&events).iter().map(|worker| worker.pane).collect();
        assert_eq!(panes, vec![4, 6]);
    }

    #[test]
    fn answers_list_every_answered_prompt_in_order() {
        let lines = answers(&sample());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["paneId"], "4");
        assert_eq!(lines[0]["label"], "fix");
        assert_eq!(lines[0]["text"], "1");
        assert_eq!(lines[0]["ts"], 4);
        assert_eq!(lines[0]["screen"], "Allow edit?\n1. Yes");
    }

    #[test]
    fn a_sent_text_is_clipped_to_200_characters() {
        assert_eq!(clip(&"あ".repeat(300)).chars().count(), TEXT_CHARS);
        assert_eq!(clip("short"), "short");
    }
}
