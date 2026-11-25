//! A fix request (修正を依頼, L1-7 item 3): failed criteria and defects the
//! person picked go, as a task file in the verification's reply dir, to a
//! coding agent started on them in a new Noa tab in the target's checkout
//! (`workdir`). The tab is the person's: it opens with focus and without
//! auto approval (the agent edits the work tree), unlike the
//! verification's own tab. Nothing is written to the verification's log.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use verify::evidence;
use verify::model::export::{self, FIX_TAIL_LINES};
use verify::model::types::{AgentKind, EvidenceBody, EvidenceKey, Session, Subject};
use verify::store;

use super::ipc::{Connect, Noa, PaneSpec, Placement};
use super::runner::login_shell;

/// What the screen asks for: these failed criteria and defects of the
/// verification stored in `dir`, fixed by `agent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixRequest {
    pub session: Arc<Session>,
    /// The verification's store directory.
    pub dir: PathBuf,
    pub agent: AgentKind,
    /// Failed criterion ids.
    pub criteria: Vec<String>,
    /// Defect ids.
    pub defects: Vec<String>,
}

/// The variable the fixing agent's prompt reaches its pane's shell in.
pub const PROMPT_VAR: &str = "NOA_VERIFY_FIX_PROMPT";

/// The one-line prompt that points the agent at the task file.
pub fn prompt(task: &Path) -> String {
    format!(
        "Read {} and fix the failed acceptance criteria and defects it lists, in this \
         working tree. It is the outcome of a multi-agent verification.",
        task.display()
    )
}

/// The argv of the fixing agent's pane: the login shell (as the agents are
/// started, so their PATH is the person's) running the agent on the prompt
/// in [`PROMPT_VAR`]. The code the shell runs is fixed and holds no path:
/// `"$VAR"` is one word, as is, in sh, bash, zsh and fish alike.
pub fn argv(kind: AgentKind) -> Vec<String> {
    // agy takes a prompt to start an interactive session with only after -i.
    let interactive = if kind == AgentKind::Agy { " -i" } else { "" };
    let script = format!("exec {}{interactive} \"${PROMPT_VAR}\"", kind.command());
    vec![login_shell(), "-l".into(), "-i".into(), "-c".into(), script]
}

/// `ids` for a notice: the first few, then how many more.
fn named(ids: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut out = ids
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if ids.len() > SHOWN {
        out.push_str(&format!(" ほか {} 件", ids.len() - SHOWN));
    }
    out
}

/// Every evidence key the task for `criteria` and `defects` may show: what
/// the rulings on those criteria and every result on them cite, every
/// cross-check's evidence (a reproduced fail or defect backs a failure),
/// and the evidence of the defects' raw reports. A superset of what
/// [`export::fix_task`] lists, so it finds a tail for each it shows.
fn cited(session: &Session, criteria: &[String], defects: &[String]) -> Vec<EvidenceKey> {
    let mut keys: Vec<EvidenceKey> = Vec::new();
    let mut add = |key: &EvidenceKey| {
        if !keys.contains(key) {
            keys.push(key.clone());
        }
    };
    for id in criteria {
        for ruling in session.rulings.get(id).into_iter().flatten() {
            ruling.evidence.iter().for_each(&mut add);
        }
        for result in session.results.iter().filter(|r| &r.criterion == id) {
            result.evidence.iter().for_each(&mut add);
        }
    }
    for check in &session.cross_checks {
        let about_scope = match &check.subject {
            Subject::Result { criterion, .. } => criteria.contains(criterion),
            Subject::Defect { .. } => true,
        };
        if about_scope {
            check.evidence.iter().for_each(&mut add);
        }
    }
    for defect in session.defects.iter().filter(|d| defects.contains(&d.id)) {
        for raw in defect.sources.iter().filter_map(|k| session.raw_defect(k)) {
            raw.evidence.iter().for_each(&mut add);
        }
    }
    keys
}

/// The last [`FIX_TAIL_LINES`] lines of the stored log of every command
/// among `keys`, read from the judge's reply dir; a log that cannot be
/// read is left out (the task still names its stored copy).
fn tails(session: &Session, keys: &[EvidenceKey]) -> BTreeMap<EvidenceKey, String> {
    let reply_base = store::reply_base(session);
    let reply_dir = store::reply_dir(session);
    keys.iter()
        .filter_map(|key| {
            let item = session.evidence_item(key)?;
            let EvidenceBody::Command { .. } = item.body else {
                return None;
            };
            let stored = item.stored.as_ref()?;
            let tail = evidence::tail(&reply_base, &reply_dir, stored, FIX_TAIL_LINES).ok()?;
            Some((key.clone(), tail))
        })
        .collect()
}

/// Carries out `req`: writes the task file, then starts the agent on it.
/// `Ok` and `Err` both say what happened, for a notice. Refused, with
/// nothing written or started, for a log that is not its folder's, with
/// none of the criteria or defects, or without a link to Noa. `seen`
/// takes the version of the Noa the request reached, so the window shows
/// what that Noa allows.
pub fn request<C: Connect>(
    connector: &mut C,
    req: &FixRequest,
    seen: &mut Option<(u64, String)>,
) -> Result<String, String> {
    let session = &*req.session;
    // The log must be the folder's, its paths its own, before anything
    // below the reply dir is made (a log's id or paths edited by hand
    // would otherwise point the task elsewhere).
    verify::engine::check_store_paths(session, &req.dir)
        .map_err(|why| format!("修正を依頼できません: {why}"))?;
    // In the verification's order, whatever order they were asked in.
    let criteria: Vec<String> = session
        .criteria
        .iter()
        .filter(|c| req.criteria.contains(&c.id))
        .map(|c| c.id.clone())
        .collect();
    let defects: Vec<String> = session
        .ranked()
        .into_iter()
        .filter(|d| req.defects.contains(&d.id))
        .map(|d| d.id.clone())
        .collect();
    if criteria.is_empty() && defects.is_empty() {
        return Err("修正を依頼する基準も不具合もありません".to_owned());
    }
    let mut conn = connector
        .connect()
        .map_err(|why| format!("Noa に接続できないため、修正を依頼できません: {why}"))?;
    // Judged on this connection's own hello (the one that opens the tab):
    // an older Noa would give the fix tab the global auto-approve setting.
    // Refused before the task is written.
    let version = conn.server_version();
    *seen = Some((conn.hello_seq(), version.clone()));
    if !super::ipc::sets_tab_auto_approve(&version) {
        return Err(super::ipc::no_tab_auto_approve(&version));
    }
    let tails = tails(session, &cited(session, &criteria, &defects));
    let text = export::fix_task(session, &criteria, &defects, &tails);
    let task = store::write_fix_task(&req.dir, session, &text)
        .map_err(|e| format!("修正の依頼を書けません: {e}"))?;
    let spec = PaneSpec {
        argv: argv(req.agent),
        env: vec![(PROMPT_VAR.to_owned(), prompt(&task))],
        cwd: session.workdir.clone(),
        // A label of its own per request (its task file's name), so a
        // pane of an earlier one is never mistaken for it, nor for one of
        // the verification's own panes (`ver:<id>:<part>`).
        label: format!(
            "fix · {} · {}",
            session.id,
            task.file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        ),
        attended: true,
    };
    let ids: Vec<String> = criteria.into_iter().chain(defects).collect();
    match conn.spawn(Placement::NewTab, &spec) {
        Ok(_) => Ok(format!(
            "{} に {} の修正を依頼しました（新しいタブ）",
            req.agent.command(),
            named(&ids)
        )),
        Err(err) => Err(format!(
            "修正用のペインを開けません: {err}（依頼は {} に書きました）",
            task.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use verify::model::types::SessionId;

    use crate::session::ipc::{Changes, NoaError, PaneView};
    use crate::session::runner::tests::{finished_with_a_failure, temp_root};

    #[derive(Default)]
    struct World {
        down: bool,
        refuse: bool,
        /// The version the fake Noa's hello gives (0.9.13 when unset).
        version: Option<String>,
        spawned: Vec<(Placement, PaneSpec)>,
    }

    #[derive(Clone, Default)]
    struct FakeNoa(Rc<RefCell<World>>);

    impl Connect for FakeNoa {
        type Conn = FakeNoa;
        fn connect(&mut self) -> Result<FakeNoa, String> {
            if self.0.borrow().down {
                return Err("server off".into());
            }
            Ok(self.clone())
        }
    }

    impl Noa for FakeNoa {
        fn panes(&mut self) -> Result<Vec<PaneView>, NoaError> {
            Ok(Vec::new())
        }
        fn changes(&mut self, _: Duration) -> Result<Option<Changes>, NoaError> {
            Ok(None)
        }
        fn spawn(&mut self, placement: Placement, spec: &PaneSpec) -> Result<u64, NoaError> {
            let mut w = self.0.borrow_mut();
            if w.refuse {
                return Err(NoaError::Refused("no window".into()));
            }
            w.spawned.push((placement, spec.clone()));
            Ok(9)
        }
        fn paste_ready(&mut self, _: u64) -> Result<bool, NoaError> {
            Ok(true)
        }
        fn screen(&mut self, _: u64) -> Result<String, NoaError> {
            Ok(String::new())
        }
        fn type_text(&mut self, _: u64, _: &str, _: bool) -> Result<(), NoaError> {
            Ok(())
        }
        fn focus(&mut self, _: u64) -> Result<(), NoaError> {
            Ok(())
        }
        fn close_pane(&mut self, _: u64) -> Result<(), NoaError> {
            Ok(())
        }
        fn server_version(&self) -> String {
            self.0
                .borrow()
                .version
                .clone()
                .unwrap_or_else(|| "0.9.13".into())
        }
        fn supports_board_tab(&self) -> bool {
            true
        }

        fn hello_seq(&self) -> u64 {
            7
        }
    }

    /// A fix request on the verification stored in `dir`.
    fn request_on(dir: &Path, criteria: &[&str], defects: &[&str]) -> FixRequest {
        FixRequest {
            session: Arc::new(store::read_session(dir).unwrap()),
            dir: dir.to_owned(),
            agent: AgentKind::Codex,
            criteria: criteria.iter().map(|s| (*s).to_owned()).collect(),
            defects: defects.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn request_with(noa: &FakeNoa, req: &FixRequest) -> Result<String, String> {
        let mut connector = noa.clone();
        super::request(&mut connector, req, &mut None)
    }

    fn fix_files(req: &FixRequest) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(store::reply_dir(&req.session))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("fix-"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn ac24_a_request_writes_its_task_with_the_stored_log_tails_and_opens_an_attended_tab() {
        let root = temp_root();
        let dir = finished_with_a_failure(&root);
        let noa = FakeNoa::default();
        let req = request_on(&dir, &["AC-1"], &["D1"]);
        let notice = request_with(&noa, &req).unwrap();
        assert_eq!(
            notice,
            "codex に AC-1, D1 の修正を依頼しました（新しいタブ）"
        );
        let w = noa.0.borrow();
        assert_eq!(w.spawned.len(), 1);
        let (placement, spec) = &w.spawned[0];
        assert_eq!(*placement, Placement::NewTab);
        assert!(spec.attended, "the fix tab runs with auto approval off");
        assert_eq!(spec.cwd, req.session.workdir);
        assert!(
            spec.label
                .starts_with(&format!("fix · {} · fix-1", req.session.id)),
            "{}",
            spec.label
        );
        let task = store::reply_dir(&req.session).join("fix-1.md");
        let text = std::fs::read_to_string(&task).unwrap();
        assert!(
            text.contains("### AC-1 [must] The library builds."),
            "{text}"
        );
        assert!(
            text.contains("- claude (R1) reported fail: cargo build exits 101."),
            "{text}"
        );
        // The last 80 lines of the stored log, read from the reply dir.
        assert!(text.contains("error line 21\n"), "{text}");
        assert!(text.contains("error line 100"), "{text}");
        assert!(!text.contains("error line 20\n"), "{text}");
        assert!(
            text.contains("### D1 [high] one() does not build"),
            "{text}"
        );
        assert!(text.contains("1. cargo build"), "{text}");
        assert!(text.contains("read the first error"), "{text}");
        assert!(text.contains("ran in copies of it"), "{text}");
        assert!(text.contains("Do not push"), "{text}");
        assert_eq!(
            spec.argv.last().unwrap(),
            "exec codex \"$NOA_VERIFY_FIX_PROMPT\""
        );
        assert_eq!(spec.env, [(PROMPT_VAR.to_owned(), prompt(&task))]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_second_request_never_writes_over_the_first() {
        let root = temp_root();
        let dir = finished_with_a_failure(&root);
        let noa = FakeNoa::default();
        request_with(&noa, &request_on(&dir, &["AC-1"], &[])).unwrap();
        request_with(&noa, &request_on(&dir, &[], &["D1"])).unwrap();
        let req = request_on(&dir, &[], &[]);
        assert_eq!(fix_files(&req), ["fix-1.md", "fix-2.md"]);
        let reply = store::reply_dir(&req.session);
        let first = std::fs::read_to_string(reply.join("fix-1.md")).unwrap();
        let second = std::fs::read_to_string(reply.join("fix-2.md")).unwrap();
        assert!(first.contains("### AC-1 ") && !first.contains("### D1 "));
        assert!(second.contains("### D1 ") && !second.contains("### AC-1 "));
        let w = noa.0.borrow();
        assert_ne!(w.spawned[0].1.label, w.spawned[1].1.label);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn refusals_write_and_start_nothing() {
        let root = temp_root();
        let dir = finished_with_a_failure(&root);
        let noa = FakeNoa::default();
        // None of the criteria or defects of the verification.
        let none = request_on(&dir, &["AC-9"], &["D9"]);
        assert!(
            request_with(&noa, &none)
                .unwrap_err()
                .contains("基準も不具合もありません")
        );
        // Noa out of reach.
        let down = FakeNoa::default();
        down.0.borrow_mut().down = true;
        let req = request_on(&dir, &["AC-1"], &[]);
        assert!(
            request_with(&down, &req)
                .unwrap_err()
                .contains("Noa に接続できない")
        );
        assert!(noa.0.borrow().spawned.is_empty());
        assert!(fix_files(&req).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_noa_that_cannot_stop_a_tabs_auto_approval_gets_no_task_and_no_tab() {
        let root = temp_root();
        let dir = finished_with_a_failure(&root);
        let noa = FakeNoa::default();
        // The connection that would open the tab is a 0.9.12 (whatever a
        // look at startup saw), or names its version loosely.
        for version in ["0.9.12", "0.9.13.invalid"] {
            noa.0.borrow_mut().version = Some(version.into());
            let mut seen = None;
            let why = super::request(
                &mut noa.clone(),
                &request_on(&dir, &["AC-1"], &[]),
                &mut seen,
            )
            .unwrap_err();
            assert_eq!(why, crate::session::ipc::no_tab_auto_approve(version));
            assert_eq!(seen, Some((7, version.to_owned())));
        }
        assert!(noa.0.borrow().spawned.is_empty());
        assert!(fix_files(&request_on(&dir, &[], &[])).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_refused_tab_says_where_the_task_was_written() {
        let root = temp_root();
        let dir = finished_with_a_failure(&root);
        let noa = FakeNoa::default();
        noa.0.borrow_mut().refuse = true;
        let why = request_with(&noa, &request_on(&dir, &["AC-1"], &[])).unwrap_err();
        assert!(why.contains("修正用のペインを開けません"), "{why}");
        assert!(why.contains("fix-1.md"), "{why}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_log_that_is_not_its_folders_writes_and_starts_nothing() {
        let root = temp_root();
        let dir = finished_with_a_failure(&root);
        let noa = FakeNoa::default();
        let mut req = request_on(&dir, &["AC-1"], &[]);
        let mut session = (*req.session).clone();
        session.id = SessionId::new("ver-elsewhere");
        req.session = Arc::new(session);
        let why = request_with(&noa, &req).unwrap_err();
        assert!(why.contains("フォルダ名と一致しません"), "{why}");
        assert!(noa.0.borrow().spawned.is_empty());
        assert!(fix_files(&request_on(&dir, &[], &[])).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_prompt_reaches_the_agent_as_written_whatever_the_path_holds() {
        // Quotes, backslashes (two in a row too), a dollar, a newline.
        let task = Path::new("/tmp/it's \\\\ \"q\" $HOME\nx/fix-1.md");
        let wanted = prompt(task);
        assert!(
            argv(AgentKind::Agy)
                .last()
                .unwrap()
                .starts_with("exec agy -i \"")
        );
        let mut ran = 0;
        for shell in ["/bin/sh", "/bin/zsh", "/bin/bash", "/opt/homebrew/bin/fish"] {
            if !Path::new(shell).exists() {
                continue;
            }
            // The script the pane runs, with the agent swapped for printf.
            let script =
                argv(AgentKind::Codex)
                    .pop()
                    .unwrap()
                    .replacen("exec codex", "printf %s", 1);
            let out = std::process::Command::new(shell)
                .arg("-c")
                .arg(&script)
                .env(PROMPT_VAR, &wanted)
                .output()
                .unwrap();
            assert_eq!(String::from_utf8(out.stdout).unwrap(), wanted, "{shell}");
            ran += 1;
        }
        assert!(ran > 0);
    }
}
