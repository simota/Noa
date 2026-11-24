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
