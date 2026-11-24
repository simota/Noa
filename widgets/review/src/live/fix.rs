//! A fix request: findings the person picked go, as a task file in the
//! review's reply dir, to a coding agent started on them in a new Noa tab
//! in the review's workdir. The tab is the person's: it opens with focus
//! and without auto approval (the agent edits the work tree), unlike the
//! review's own tab. Nothing is written to the review's log.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use review::model::export;
use review::model::types::{AgentKind, Session};
use review::store;

use super::ipc::{Connect, Noa, PaneSpec, Placement};
use super::runner::login_shell;

/// What the screen asks for: these findings of the review stored in
/// `dir`, fixed by `agent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixRequest {
    pub session: Arc<Session>,
    /// The review's store directory.
    pub dir: PathBuf,
    pub agent: AgentKind,
    pub ids: Vec<String>,
}

/// `text` as one single-quoted word, read the same by sh, bash, zsh and
/// fish (`'\''` ends the quote, adds an escaped quote and opens another).
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The argv of the fixing agent's pane: the login shell (as the agents are
/// started, so their PATH is the person's) running the agent on a one-line
/// prompt that points at the task file.
pub fn argv(kind: AgentKind, task: &Path) -> Vec<String> {
    let prompt = format!(
        "Read {} and fix the review findings it lists, in this working tree. \
         It is the outcome of a multi-agent code review.",
        task.display()
    );
    // agy takes a prompt to start an interactive session with only after -i.
    let interactive = if kind == AgentKind::Agy { " -i" } else { "" };
    let script = format!("exec {}{interactive} {}", kind.command(), quoted(&prompt));
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

/// Carries out `req`: writes the task file, then starts the agent on it.
/// `Ok` and `Err` both say what happened, for a notice. Refused, with
/// nothing written or started, for a review without a tree to fix, with
/// none of the findings, or without a link to Noa.
pub fn request<C: Connect>(connector: &mut C, req: &FixRequest) -> Result<String, String> {
    let session = &*req.session;
    let workdir = store::fix_workdir(session)?.to_path_buf();
    let findings = export::fix_findings(session, &req.ids);
    if findings.is_empty() {
        return Err("修正を依頼する指摘がありません".to_owned());
    }
    let ids: Vec<String> = findings.iter().map(|f| f.id.clone()).collect();
    let mut conn = connector
        .connect()
        .map_err(|why| format!("Noa に接続できないため、修正を依頼できません: {why}"))?;
    let snapshot = store::target_dir(&store::reply_dir(&req.dir, session));
    let text = export::fix_task(session, &ids, &snapshot);
    let task = store::write_fix_task(&req.dir, session, &text)
        .map_err(|e| format!("修正の依頼を書けません: {e}"))?;
    let spec = PaneSpec {
        argv: argv(req.agent, &task),
        cwd: workdir,
        // A label of its own per request (its task file's name), so a
        // pane of an earlier one is never mistaken for it.
        label: format!(
            "fix · {} · {}",
            session.id,
            task.file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        ),
        attended: true,
    };
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

    use review::model::types::TargetKind;

    use crate::session::ipc::{Changes, NoaError, PaneView};

    #[derive(Default)]
    struct World {
        down: bool,
        refuse: bool,
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
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("noa-review-fix-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The finished demo review with its workdir at `workdir`.
    fn request(workdir: &Path, ids: &[&str]) -> FixRequest {
        let mut session = crate::demo::session();
        session.workdir = Some(workdir.to_owned());
        FixRequest {
            session: Arc::new(session),
            dir: workdir.join("store"),
            agent: AgentKind::Codex,
            ids: ids.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn a_request_writes_its_task_and_opens_an_attended_tab_in_the_workdir() {
        let workdir = temp("ok");
        let noa = FakeNoa::default();
        let req = request(&workdir, &["F1", "F2"]);
        // Ranked (L2-4), whatever order they were asked in.
        let ranked: Vec<&str> = req
            .session
            .ranked()
            .into_iter()
            .map(|f| f.id.as_str())
            .filter(|id| ["F1", "F2"].contains(id))
            .collect();
        let notice = request_with(&noa, &req).unwrap();
        assert!(
            notice.contains(&format!(
                "codex に {} の修正を依頼しました",
                ranked.join(", ")
            )),
            "{notice}"
        );
        let w = noa.0.borrow();
        assert_eq!(w.spawned.len(), 1);
        let (placement, spec) = &w.spawned[0];
        assert_eq!(*placement, Placement::NewTab);
        assert!(spec.attended);
        assert_eq!(spec.cwd, workdir);
        assert!(spec.label.starts_with("fix · rev-demo"), "{}", spec.label);
        let task = store::workdir_reply_dir(&workdir, &req.session.id).join("fix-1.md");
        let text = std::fs::read_to_string(&task).unwrap();
        let at = |id: &str| text.find(&format!("### {id} ")).unwrap();
        assert!(at(ranked[0]) < at(ranked[1]));
        assert!(text.contains("Do not push"));
        assert!(text.contains("stop and ask before changing anything"));
        assert!(
            spec.argv
                .last()
                .unwrap()
                .contains(&task.display().to_string())
        );
        assert!(spec.argv.last().unwrap().starts_with("exec codex '"));
    }

    fn request_with(noa: &FakeNoa, req: &FixRequest) -> Result<String, String> {
        let mut connector = noa.clone();
        super::request(&mut connector, req)
    }

    #[test]
    fn a_second_request_never_writes_over_the_first() {
        let workdir = temp("again");
        let noa = FakeNoa::default();
        request_with(&noa, &request(&workdir, &["F1"])).unwrap();
        request_with(&noa, &request(&workdir, &["F2"])).unwrap();
        let reply = store::workdir_reply_dir(&workdir, &request(&workdir, &[]).session.id);
        let first = std::fs::read_to_string(reply.join("fix-1.md")).unwrap();
        let second = std::fs::read_to_string(reply.join("fix-2.md")).unwrap();
        assert!(first.contains("### F1 ") && !first.contains("### F2 "));
        assert!(second.contains("### F2 ") && !second.contains("### F1 "));
        let w = noa.0.borrow();
        assert_ne!(w.spawned[0].1.label, w.spawned[1].1.label);
    }

    #[test]
    fn refusals_write_and_start_nothing() {
        let workdir = temp("refused");
        let reply = |req: &FixRequest| store::workdir_reply_dir(&workdir, &req.session.id);

        // No findings of the review.
        let noa = FakeNoa::default();
        let none = request(&workdir, &["F99"]);
        assert!(
            request_with(&noa, &none)
                .unwrap_err()
                .contains("指摘がありません")
        );

        // No workdir.
        let mut session = (*request(&workdir, &["F1"]).session).clone();
        session.workdir = None;
        let no_workdir = FixRequest {
            session: Arc::new(session.clone()),
            ..request(&workdir, &["F1"])
        };
        assert!(
            request_with(&noa, &no_workdir)
                .unwrap_err()
                .contains("作業ディレクトリ")
        );

        // A document review.
        session.workdir = Some(workdir.clone());
        session.target.kind = TargetKind::Document;
        let document = FixRequest {
            session: Arc::new(session),
            ..request(&workdir, &["F1"])
        };
        assert!(request_with(&noa, &document).unwrap_err().contains("文書"));

        // Noa out of reach.
        let down = FakeNoa::default();
        down.0.borrow_mut().down = true;
        let req = request(&workdir, &["F1"]);
        assert!(
            request_with(&down, &req)
                .unwrap_err()
                .contains("Noa に接続できない")
        );

        assert!(noa.0.borrow().spawned.is_empty());
        assert!(!reply(&req).join("fix-1.md").exists());
    }

    #[test]
    fn a_refused_tab_says_where_the_task_was_written() {
        let workdir = temp("tab");
        let noa = FakeNoa::default();
        noa.0.borrow_mut().refuse = true;
        let req = request(&workdir, &["F1"]);
        let why = request_with(&noa, &req).unwrap_err();
        assert!(why.contains("修正用のペインを開けません"), "{why}");
        assert!(why.contains("fix-1.md"), "{why}");
    }

    #[test]
    fn a_reply_dir_behind_a_link_is_refused() {
        let workdir = temp("link");
        let elsewhere = temp("link-elsewhere");
        std::os::unix::fs::symlink(&elsewhere, workdir.join(".noa")).unwrap();
        let noa = FakeNoa::default();
        let why = request_with(&noa, &request(&workdir, &["F1"])).unwrap_err();
        assert!(why.contains("書けません"), "{why}");
        assert!(noa.0.borrow().spawned.is_empty());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn the_task_says_what_was_reviewed_for_a_ref_the_work_tree_and_files() {
        let snapshot = Path::new("/w/.noa/reviews/rev-demo/target");
        let mut session = crate::demo::session();
        let ids = vec!["F1".to_owned()];
        let text = export::fix_task(&session, &ids, snapshot);
        assert!(text.contains("feat/store-rewrite `"), "{text}");
        assert!(text.contains("stop and ask before changing anything"));
        assert!(text.contains("/w/.noa/reviews/rev-demo/target"));
        assert!(text.contains("## The lead's verdict: request_changes"));

        session.manifest.head_ref = None;
        let text = export::fix_task(&session, &ids, snapshot);
        assert!(text.contains("the work tree (on `"), "{text}");
        assert!(text.contains("it may have changed since"));
        assert!(!text.contains("stop and ask"));

        session.target.kind = TargetKind::Files;
        let text = export::fix_task(&session, &ids, snapshot);
        assert!(text.contains("files of this folder"), "{text}");
    }

    #[test]
    fn the_prompt_survives_quotes_in_the_path() {
        let task = Path::new("/tmp/it's here/fix-1.md");
        let script = argv(AgentKind::Agy, task).pop().unwrap();
        assert!(script.starts_with("exec agy -i '"), "{script}");
        assert!(script.contains(r"it'\''s here"), "{script}");
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script.replacen("exec agy -i", "printf %s", 1))
            .output()
            .unwrap();
        let printed = String::from_utf8(out.stdout).unwrap();
        assert!(
            printed.starts_with("Read /tmp/it's here/fix-1.md and fix"),
            "{printed}"
        );
    }
}
