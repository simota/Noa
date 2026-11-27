//! What the screen shows ([`State`]) and where it comes from: [`Live`] over
//! a repo and the widget's data folder, or the canned demo. Every action
//! the screen takes goes through [`Backend::act`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use scaffold::gitread;
use scaffold::handoff::{self, Related};
use scaffold::model::{
    Asset, Decision, EvidenceRef, Improvement, Lifecycle, Observation, SessionFact, Subject,
};
use scaffold::registry::{self, LoadError, Loaded, Registry};
use scaffold::scan::{self, Report, SourceReport, SourceState};
use scaffold::setup::{self, Setup};
use scaffold::store::{self, Event, LinkKind, Store};
use scaffold::time::{self, Millis};
use scaffold::verify_log;

/// Everything one scan gives the screen.
#[derive(Debug, Clone)]
pub struct State {
    pub repo: PathBuf,
    pub commit: Option<String>,
    pub scanned_at: Millis,
    pub setup: Setup,
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

    /// Detected files that the person has not yet added to the asset ledger.
    pub fn pending_assets(&self) -> Vec<Asset> {
        let assets = match &self.registry {
            Ok(loaded) => loaded.registry.assets.as_slice(),
            Err(LoadError::Absent) => &[],
            Err(LoadError::Invalid(_)) => return Vec::new(),
        };
        self.setup
            .assets
            .iter()
            .filter(|candidate| {
                !assets
                    .iter()
                    .any(|asset| asset.id == candidate.id || asset.path == candidate.path)
            })
            .cloned()
            .collect()
    }

    pub fn setup_task(&self) -> Option<(String, PathBuf)> {
        task_for(
            &self.observations,
            &Subject::Improvement(self.setup.task_key()),
        )
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
    RegisterAssets,
    SetupTask,
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
        // Resolve a project alias before finding its worktree root.
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let repo = gitread::repo_root(&dir);
        Live {
            store: data_dir.map(|d| Store::new(d, &repo)),
            identity: gitread::repo_identity(&repo),
            repo,
        }
    }

    fn store(&self) -> Result<&Store, String> {
        self.store
            .as_ref()
            .ok_or_else(|| "データフォルダがないため記録できません".to_owned())
    }
}

fn task_for(observations: &[Observation], subject: &Subject) -> Option<(String, PathBuf)> {
    observations
        .iter()
        .filter_map(|observation| match observation {
            Observation::Task {
                subject: s,
                id,
                path,
                at,
            } if s == subject => Some((*at, id.clone(), path.clone())),
            _ => None,
        })
        .max_by_key(|(at, ..)| *at)
        .map(|(_, id, path)| (id, path))
}

/// General projects show their setup sources, not missing Noa internals.
/// The registration checker's own report and parity contract stay intact.
fn report_for(repo: &Path, setup: &Setup, registry: &Result<Loaded, LoadError>) -> Report {
    if setup.registration_applicable {
        return scan::scan(repo);
    }
    let mut sources = setup.sources.clone();
    sources.retain(|source| source.path != scan::REGISTRY);
    match registry {
        Ok(_) => sources.push(SourceReport {
            path: scan::REGISTRY.into(),
            state: SourceState::Ok,
            error: None,
        }),
        Err(LoadError::Invalid(error)) => sources.push(SourceReport {
            path: scan::REGISTRY.into(),
            state: SourceState::Unknown,
            error: Some(error.clone()),
        }),
        Err(LoadError::Absent) => {}
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    let unknown = sources
        .iter()
        .any(|source| source.state == SourceState::Unknown);
    Report {
        schema: 1,
        scope: "開発環境の入口となるファイルを検出しました。内容の妥当性・インストール状態・実行結果は未確認です。".into(),
        status: if unknown { "unknown" } else { "consistent" }.into(),
        exit: if unknown { 2 } else { 0 },
        sources, widgets: Vec::new(), findings: Vec::new(), explained: Vec::new(),
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
        let setup = setup::scan(&self.repo);
        let commit = gitread::head_commit(&self.repo);
        let registry = registry::load(&self.repo);
        let report = report_for(&self.repo, &setup, &registry);
        let mut problem = None;
        let improvements = if !setup.registration_applicable
            || matches!(registry, Err(LoadError::Absent))
        {
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
        let mut observations = self
            .store
            .as_ref()
            .map(Store::observations)
            .unwrap_or_default();
        let subject = Subject::Improvement(setup.task_key());
        // The task file is authoritative: an absent or moved task can be
        // recreated, and a task survives an interrupted observation write.
        observations
            .retain(|o| !matches!(o, Observation::Task { subject: s, .. } if *s == subject));
        match handoff::find_setup_task(&self.repo, &setup.task_key()) {
            Ok(Some((id, path))) => observations.push(Observation::Task {
                subject,
                id,
                path,
                at: now,
            }),
            Ok(None) => {}
            Err(error) => problem = Some(format!("セットアップタスクを確認できません: {error}")),
        }
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
            setup,
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
        match action {
            Action::RegisterAssets => {
                let pending = state.pending_assets();
                if pending.is_empty() {
                    return Err("追加する資産はありません。登録済みの資産を確認してください".into());
                }
                let current = setup::scan(&self.repo);
                if pending.iter().any(|asset| !current.assets.contains(asset)) {
                    return Err(
                        "登録候補のファイルが変更されました。再スキャンして内容を確認してください"
                            .into(),
                    );
                }
                let count = pending.len();
                match &state.registry {
                    Ok(loaded) => {
                        let mut next = loaded.registry.clone();
                        next.assets.extend(pending);
                        registry::save(&self.repo, loaded, &next)?;
                    }
                    Err(LoadError::Absent) => {
                        registry::create(
                            &self.repo,
                            &Registry {
                                schema: 1,
                                widgets: BTreeMap::new(),
                                assets: pending,
                            },
                        )?;
                    }
                    Err(error) => return Err(error.to_string()),
                }
                Ok(format!(
                    "{count} 件を資産に登録しました。候補として記録し、検証結果や採用判断を残せます"
                ))
            }
            Action::SetupTask => {
                if state.setup.attention_count() == 0 {
                    return Err("不足しているセットアップ項目はありません".into());
                }
                if setup::scan(&self.repo).task_key() != state.setup.task_key() {
                    return Err("セットアップの状態が変わりました。再スキャンして作業指示を確認してください".into());
                }
                let (id, path, created) = handoff::hand_off_setup(&self.repo, &state.setup, now)?;
                if let Some(store) = &self.store {
                    let subject = Subject::Improvement(state.setup.task_key());
                    if task_for(&store.observations(), &subject)
                        .as_ref()
                        .map(|(id, _)| id)
                        != Some(&id)
                        && let Err(error) = store.append(&Event::Linked {
                            subject,
                            kind: LinkKind::Task,
                            r#ref: id.clone(),
                            path,
                            at: now,
                        })
                    {
                        return Ok(format!(
                            "タスク {id} は保存しました。関連付けの保存に失敗しましたが、View → Tasks で続けられます: {error}"
                        ));
                    }
                }
                if created {
                    Ok(format!(
                        "タスク {id} を作成しました。View → Tasks で作業指示を確認し、エージェントに渡せます"
                    ))
                } else {
                    Ok(format!(
                        "タスク {id} は作成済みです。View → Tasks で続けてください"
                    ))
                }
            }
            Action::HandOff { improvement } => {
                let store = self.store()?;
                let imp = state
                    .improvements
                    .iter()
                    .find(|i| i.id == improvement)
                    .ok_or("その候補はもうありません")?;
                if imp.resolved.is_some() {
                    return Err("解消済みの候補はタスクにしません".into());
                }
                if let Some((id, _)) =
                    task_for(&store.observations(), &Subject::Improvement(improvement))
                {
                    return Ok(format!(
                        "タスク {id} は作成済みです。View → Tasks で続けてください"
                    ));
                }
                let id = handoff::hand_off(store, &self.repo, imp, &related(state, &imp.id), now)?;
                Ok(format!(
                    "タスク {id} を作りました。View → Tasks で続けてください"
                ))
            }
            Action::Link { subject, kind, id } => {
                self.store()?.link_session(subject, kind, &id, now)?;
                Ok(format!("{} {} を関連付けました", kind.label(), id.trim()))
            }
            Action::Unlink { subject, kind, id } => {
                self.store()?.unlink(subject, kind, &id, now)?;
                Ok(format!("{} {id} を外しました", kind.label()))
            }
            Action::Skip { subject, reason } => {
                self.store()?
                    .skip(subject, &reason, state.commit.clone(), now)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn put(repo: &Path, path: &str, text: &str) {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    fn task_count(repo: &Path) -> usize {
        std::fs::read_dir(repo.join(".noa/tasks"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("t-"))
            .count()
    }

    #[test]
    fn a_generic_project_starts_without_a_registry_or_noa_specific_errors() {
        let repo = tempfile::tempdir().unwrap();
        put(
            repo.path(),
            "README.md",
            "# Example\n\nDevelopment instructions.\n",
        );
        let state = Live::new(repo.path().to_owned(), None).scan();
        // The platform's temp directory can itself be an alias (macOS /var).
        assert_eq!(state.repo, repo.path().canonicalize().unwrap());
        assert!(state.unregistered());
        assert!(!state.setup.registration_applicable);
        assert!(!state.setup.checks.is_empty());
        assert!(
            state
                .report
                .sources
                .iter()
                .all(|source| !source.path.starts_with("crates/noa-app"))
        );
        assert!(state.improvements.is_empty());
        assert!(!repo.path().join("dev").exists());
        assert!(!repo.path().join(".noa").exists());
    }

    #[test]
    fn project_and_parent_aliases_open_the_same_repository() {
        let workspace = tempfile::tempdir().unwrap();
        let repo = workspace.path().join("project");
        let aliases = tempfile::tempdir().unwrap();
        put(&repo, "README.md", "# Project\n");
        let alias = aliases.path().join("project");
        std::os::unix::fs::symlink(&repo, &alias).unwrap();
        let parent_alias = aliases.path().join("workspace");
        std::os::unix::fs::symlink(workspace.path(), &parent_alias).unwrap();
        let expected = repo.canonicalize().unwrap();
        for dir in [repo, alias, parent_alias.join("project")] {
            let state = Live::new(dir, None).scan();
            assert_eq!(state.repo, expected);
            assert!(
                state
                    .setup
                    .sources
                    .iter()
                    .any(|source| source.path == "README.md" && source.state == SourceState::Ok)
            );
        }
    }

    #[test]
    fn discovered_assets_can_be_registered_without_a_data_folder() {
        let repo = tempfile::tempdir().unwrap();
        put(
            repo.path(),
            "README.md",
            "# Example\n\nHow to get started.\n",
        );
        put(
            repo.path(),
            "AGENTS.md",
            "# Rules\n\nRun the relevant tests.\n",
        );
        let mut backend = Live::new(repo.path().to_owned(), None);
        let before = backend.scan();
        let pending = before.pending_assets();
        assert!(pending.len() >= 2);
        backend.act(Action::RegisterAssets, &before).unwrap();
        let after = backend.scan();
        assert!(after.pending_assets().is_empty());
        let loaded = after.registry.as_ref().unwrap();
        assert_eq!(loaded.registry.assets, pending);
        assert!(
            loaded.registry.widgets.is_empty(),
            "shipping intent is not guessed"
        );
        assert!(
            loaded
                .registry
                .assets
                .iter()
                .all(|asset| asset.decisions.is_empty())
        );
        assert_eq!(
            backend.act(Action::RegisterAssets, &before),
            Err(registry::CONFLICT.into())
        );
        assert!(backend.act(Action::RegisterAssets, &after).is_err());
        assert_eq!(registry::load(repo.path()).unwrap(), *loaded);
    }

    #[test]
    fn registration_preserves_existing_decisions_and_refuses_outside_edits() {
        let repo = tempfile::tempdir().unwrap();
        put(repo.path(), "README.md", "# Example\n\nRead me.\n");
        let mut backend = Live::new(repo.path().to_owned(), None);
        let before = backend.scan();
        backend.act(Action::RegisterAssets, &before).unwrap();
        let initial = registry::load(repo.path()).unwrap();
        let mut next = initial.registry.clone();
        next.assets[0]
            .decide(Decision {
                to: Lifecycle::Trial,
                at: "2026-11-27".into(),
                reason: "Try the documented setup".into(),
                evidence: Vec::new(),
            })
            .unwrap();
        registry::save(repo.path(), &initial, &next).unwrap();
        put(repo.path(), "AGENTS.md", "# Work rules\n\nCheck changes.\n");
        let preview = backend.scan();
        let mut edited = std::fs::read(registry::path(repo.path())).unwrap();
        edited.push(b'\n');
        std::fs::write(registry::path(repo.path()), &edited).unwrap();
        assert_eq!(
            backend.act(Action::RegisterAssets, &preview),
            Err(registry::CONFLICT.into())
        );
        assert_eq!(std::fs::read(registry::path(repo.path())).unwrap(), edited);
        let fresh = backend.scan();
        backend.act(Action::RegisterAssets, &fresh).unwrap();
        assert_eq!(
            registry::load(repo.path()).unwrap().registry.assets[0].decisions,
            next.assets[0].decisions
        );
    }

    #[test]
    fn changed_previews_are_refused_before_writing() {
        let repo = tempfile::tempdir().unwrap();
        put(repo.path(), "README.md", "# Example\n");
        let mut backend = Live::new(repo.path().to_owned(), None);
        let before = backend.scan();
        std::fs::remove_file(repo.path().join("README.md")).unwrap();
        assert!(
            backend
                .act(Action::RegisterAssets, &before)
                .unwrap_err()
                .contains("再スキャン")
        );
        assert!(
            backend
                .act(Action::SetupTask, &before)
                .unwrap_err()
                .contains("再スキャン")
        );
        assert!(!registry::path(repo.path()).exists());
        assert!(!repo.path().join(".noa").exists());
    }

    #[test]
    fn setup_tasks_recover_after_restart_and_deletion_without_a_data_folder() {
        let repo = tempfile::tempdir().unwrap();
        let mut backend = Live::new(repo.path().to_owned(), None);
        let before = backend.scan();
        backend.act(Action::SetupTask, &before).unwrap();
        let (id, path) = backend.scan().setup_task().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before.setup.task_text()
        );
        let mut reopened = Live::new(repo.path().to_owned(), None);
        assert_eq!(
            reopened.scan().setup_task(),
            Some((id.clone(), path.clone()))
        );
        assert!(
            reopened
                .act(Action::SetupTask, &before)
                .unwrap()
                .contains("作成済み")
        );
        assert_eq!(task_count(repo.path()), 1);
        std::fs::remove_file(&path).unwrap();
        let after_delete = reopened.scan();
        assert!(after_delete.setup_task().is_none());
        reopened.act(Action::SetupTask, &after_delete).unwrap();
        assert_eq!(task_count(repo.path()), 1);
        assert!(reopened.scan().setup_task().is_some());
    }

    #[test]
    fn a_failed_observation_write_does_not_duplicate_a_saved_task() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path(), repo.path());
        std::fs::create_dir_all(store.dir.join(store::OBSERVATIONS_FILE)).unwrap();
        let mut backend = Live::new(repo.path().to_owned(), Some(data.path()));
        let before = backend.scan();
        assert!(
            backend
                .act(Action::SetupTask, &before)
                .unwrap()
                .contains("関連付けの保存に失敗")
        );
        let saved = backend.scan().setup_task().unwrap();
        backend.act(Action::SetupTask, &before).unwrap();
        assert_eq!(task_count(repo.path()), 1);
        assert_eq!(backend.scan().setup_task(), Some(saved));
    }

    #[test]
    fn a_structurally_invalid_registry_is_an_unknown_source() {
        let repo = tempfile::tempdir().unwrap();
        put(
            repo.path(),
            scan::REGISTRY,
            "{\"schema\":2,\"widgets\":{},\"assets\":[]}\n",
        );
        let state = Live::new(repo.path().to_owned(), None).scan();
        assert!(!state.setup.registration_applicable);
        assert!(state.pending_assets().is_empty());
        let source = state
            .report
            .sources
            .iter()
            .find(|source| source.path == scan::REGISTRY)
            .unwrap();
        assert_eq!(source.state, SourceState::Unknown);
        assert_eq!(state.report.status, "unknown");
    }
}
