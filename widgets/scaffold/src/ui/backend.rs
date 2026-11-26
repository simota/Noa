//! What the screen shows ([`State`]) and where it comes from: [`Live`] over
//! a repo and the widget's data folder, or the canned demo. Every action
//! the screen takes goes through [`Backend::act`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use scaffold::gitread;
use scaffold::handoff::{self, Related};
use scaffold::model::{
    Decision, EvidenceRef, Improvement, Lifecycle, Observation, SessionFact, Subject,
};
use scaffold::registry::{self, LoadError, Loaded};
use scaffold::scan::{self, Report};
use scaffold::store::{self, LinkKind, Store};
use scaffold::time::{self, Millis};
use scaffold::verify_log;

/// Everything one scan gives the screen.
#[derive(Debug, Clone)]
pub struct State {
    pub repo: PathBuf,
    pub commit: Option<String>,
    pub scanned_at: Millis,
    pub report: Report,
    pub registry: Result<Loaded, LoadError>,
    /// Every candidate seen, resolved ones included; empty for a repo
    /// with no registry.
    pub improvements: Vec<Improvement>,
    pub observations: Vec<Observation>,
    /// The fact of each linked Verify session, by id.
    pub sessions: BTreeMap<String, SessionFact>,
    /// What went wrong keeping the widget's own files, if anything.
    pub problem: Option<String>,
}

impl State {
    /// 未登録リポジトリ: no `dev/scaffold/registry.json`.
    pub fn unregistered(&self) -> bool {
        matches!(self.registry, Err(LoadError::Absent))
    }

    /// The links (Verify, Review, task) of `subject`.
    pub fn links(&self, subject: &Subject) -> Vec<(LinkKind, String, PathBuf)> {
        self.observations
            .iter()
            .filter(|o| o.subject() == subject)
            .filter_map(|o| match o {
                Observation::Verify { session, path, .. } => {
                    Some((LinkKind::Verify, session.clone(), path.clone()))
                }
                Observation::Review { session, path, .. } => {
                    Some((LinkKind::Review, session.clone(), path.clone()))
                }
                Observation::Task { id, path, .. } => {
                    Some((LinkKind::Task, id.clone(), path.clone()))
                }
                Observation::Skipped { .. } => None,
            })
            .collect()
    }

    /// `subject`'s observations, each Verify link with its session's fact.
    pub fn facts_of(&self, subject: &Subject) -> Vec<(Observation, Option<SessionFact>)> {
        self.observations
            .iter()
            .filter(|o| o.subject() == subject)
            .map(|o| {
                let fact = match o {
                    Observation::Verify { session, .. } => self.sessions.get(session).cloned(),
                    _ => None,
                };
                (o.clone(), fact)
            })
            .collect()
    }
}

/// One thing the person asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    HandOff {
        improvement: String,
    },
    Link {
        subject: Subject,
        kind: LinkKind,
        id: String,
    },
    Unlink {
        subject: Subject,
        kind: LinkKind,
        id: String,
    },
    Skip {
        subject: Subject,
        reason: String,
    },
    Decide {
        asset: String,
        to: Lifecycle,
        reason: String,
        evidence: Vec<EvidenceRef>,
    },
}

pub trait Backend {
    /// Scans now.
    fn scan(&mut self) -> State;
    /// Does `action`; the notice to show, or why it was refused.
    fn act(&mut self, action: Action, state: &State) -> Result<String, String>;
    fn now(&self) -> Millis;
    /// The offset from UTC the screen shows times in.
    fn offset(&self, at: Millis) -> Millis;
    fn is_demo(&self) -> bool {
        false
    }
}

/// A repo on disk and the widget's folder for it.
pub struct Live {
    repo: PathBuf,
    store: Option<Store>,
    identity: String,
}

impl Live {
    /// `dir` is any directory in the repo (Noa passes the focused pane's);
    /// the repo is its work tree's root. `data_dir` is `<data dir>`
    /// (`None`: nothing is kept).
    pub fn new(dir: PathBuf, data_dir: Option<&Path>) -> Live {
        let repo = gitread::repo_root(&dir);
        Live {
            store: data_dir.map(|d| Store::new(d, &repo)),
            identity: gitread::repo_identity(&repo),
            repo,
        }
    }
}

/// The Verify and Review links of `imp`, for its brief.
pub fn related(state: &State, imp: &str) -> Vec<Related> {
    state
        .links(&Subject::Improvement(imp.to_owned()))
        .into_iter()
        .filter(|(kind, ..)| *kind != LinkKind::Task)
        .map(|(kind, id, path)| Related { kind, id, path })
        .collect()
}

impl Backend for Live {
    fn scan(&mut self) -> State {
        let now = self.now();
        let report = scan::scan(&self.repo);
        let commit = gitread::head_commit(&self.repo);
        let registry = registry::load(&self.repo);
        let mut problem = None;
        let improvements = if matches!(registry, Err(LoadError::Absent)) {
            Vec::new()
        } else {
            match &self.store {
                Some(store) => store
                    .record_scan(&report, commit.clone(), &self.identity, now)
                    .unwrap_or_else(|e| {
                        problem = Some(format!("スキャン結果を保存できません: {e}"));
                        // Unreadable history: this scan's candidates only,
                        // shown, not saved.
                        let stored = store.improvements().unwrap_or_default();
                        store::merge(stored, &report, &self.identity, now)
                    }),
                None => {
                    problem = Some("データフォルダがないため、候補と関連付けを保存しません".into());
                    store::merge(Vec::new(), &report, &self.identity, now)
                }
            }
        };
        let observations = self
            .store
            .as_ref()
            .map(Store::observations)
            .unwrap_or_default();
        let sessions = observations
            .iter()
            .filter_map(|o| match o {
                Observation::Verify { session, path, .. } => {
                    Some((session.clone(), verify_log::fact(path)))
                }
                _ => None,
            })
            .collect();
        State {
            repo: self.repo.clone(),
            commit,
            scanned_at: now,
            report,
            registry,
            improvements,
            observations,
            sessions,
            problem,
        }
    }

    fn act(&mut self, action: Action, state: &State) -> Result<String, String> {
        let now = self.now();
        let store = self
            .store
            .as_ref()
            .ok_or("データフォルダがないため記録できません")?;
        match action {
            Action::HandOff { improvement } => {
                let imp = state
                    .improvements
                    .iter()
                    .find(|i| i.id == improvement)
                    .ok_or("その候補はもうありません")?;
                if imp.resolved.is_some() {
                    return Err("解消済みの候補はタスクにしません".into());
                }
                let id = handoff::hand_off(store, &self.repo, imp, &related(state, &imp.id), now)?;
                Ok(format!("タスク {id} を作りました（Tasks に表示されます）"))
            }
            Action::Link { subject, kind, id } => {
                store.link_session(subject, kind, &id, now)?;
                Ok(format!("{} {} を関連付けました", kind.label(), id.trim()))
            }
            Action::Unlink { subject, kind, id } => {
                store.unlink(subject, kind, &id, now)?;
                Ok(format!("{} {id} を外しました", kind.label()))
            }
            Action::Skip { subject, reason } => {
                store.skip(subject, &reason, state.commit.clone(), now)?;
                Ok("省略を記録しました".into())
            }
            Action::Decide {
                asset,
                to,
                reason,
                evidence,
            } => {
                let loaded = state.registry.as_ref().map_err(|e| e.to_string())?;
                let mut next = loaded.registry.clone();
                let a = next
                    .assets
                    .iter_mut()
                    .find(|a| a.id == asset)
                    .ok_or("その資産は registry にありません")?;
                a.decide(Decision {
                    to,
                    at: time::date(now + self.offset(now)),
                    reason: reason.trim().to_owned(),
                    evidence,
                })
                .map_err(|e| e.to_string())?;
                registry::save(&self.repo, loaded, &next)?;
                Ok(format!("{asset} を「{}」にしました", to.label()))
            }
        }
    }

    fn now(&self) -> Millis {
        time::now_ms()
    }

    fn offset(&self, at: Millis) -> Millis {
        time::local_offset(at)
    }
}
