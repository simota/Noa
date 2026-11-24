//! AC-19 through the library API: `engine::check_resumable` over a real
//! capture and real verifier worktrees (temp git repositories). A missing
//! worktree is no refusal (it is rebuilt from the recipe, and the rebuilt
//! one holds the captured bytes); one re-pointed to a branch or at another
//! commit is refused, naming the verifier and what it holds; a changed
//! snapshot or spec copy is refused. The runner's resume (labelled panes
//! taken up, gone ones started again, the rebuild itself) is tested in
//! `src/live/runner.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use verify::engine::{self, Config, Engine, Input, Setup};
use verify::model::types::{
    AgentKind, BuiltinStrategy, Head, Isolation, MAX_MAX_ROUNDS, PartId, Session, SessionId,
    StrategyChoice, Target, TargetKind, VerifierChoice, WorktreeState,
};
use verify::{capture, store, worktree};

/// Git for a test's own setup, away from the person's settings.
fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            cmd.env_remove(name);
        }
    }
    let out = cmd
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A fresh canonical folder (git reports canonical paths).
fn temp_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "noa-verify-resume-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

/// A verification captured from a real work tree (a tracked change and an
/// untracked file, two commits, a spec file) with both verifiers'
/// worktrees built from its recipe and logged as made.
struct Fixture {
    root: PathBuf,
    /// The store directory, named as the session.
    dir: PathBuf,
    session: Session,
}

impl Fixture {
    fn new() -> Fixture {
        let root = temp_root();
        let repo = root.join("repo");
        fs::create_dir_all(repo.join("src")).unwrap();
        git(&repo, &["init", "-q"]);
        fs::write(repo.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\n").unwrap();
        fs::write(repo.join("SPEC.md"), "# Spec\n\n- one() returns 2\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        fs::write(repo.join("README.md"), "readme\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "readme"]);
        // What is verified: uncommitted work.
        fs::write(repo.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").unwrap();
        fs::write(repo.join("src/new.rs"), "pub fn new() {}\n").unwrap();

        let target = Target {
            kind: TargetKind::Diff,
            repo: repo.clone(),
            base: Some("main".into()),
            head: Head::Worktree,
            paths: Vec::new(),
            spec: Some("SPEC.md".into()),
        };
        let workdir = capture::repo_root(&repo).unwrap();
        let main_worktree = worktree::main_worktree(&workdir).unwrap();
        let id = SessionId::new("ver-1");
        store::prepare_workdir(&workdir).unwrap();
        let reply_dir = store::workdir_reply_dir(&workdir, &id);
        fs::create_dir_all(&reply_dir).unwrap();
        let target_dir = store::target_dir(&reply_dir);
        let captured = capture::capture(&target, &target_dir, 1_000).unwrap();
        assert!(captured.rebuild.worktree_patch.is_some(), "work-tree mode");
        assert!(captured.spec.is_some());
        let panel: Vec<VerifierChoice> = [
            (AgentKind::Claude, BuiltinStrategy::Conformance),
            (AgentKind::Codex, BuiltinStrategy::Falsify),
        ]
        .into_iter()
        .map(|(kind, strategy)| VerifierChoice {
            agent: kind.into(),
            strategy: StrategyChoice::Builtin(strategy),
        })
        .collect();
        let dir = root.join("store").join(id.as_str());
        fs::create_dir_all(&dir).unwrap();
        let setup = Setup {
            request: "Verify one()",
            target,
            manifest: captured.manifest,
            rebuild: captured.rebuild,
            spec: captured.spec,
            isolation: Isolation::Worktree,
            user_criteria: Vec::new(),
            confirm: true,
            workdir,
            main_worktree,
            max_rounds: 2,
            round_cap: MAX_MAX_ROUNDS,
            judge: AgentKind::Claude.into(),
            panel: &panel,
        };
        let (mut engine, _) = Engine::create_in(id, setup, &dir, Config::default(), 1_000).unwrap();
        let s = engine.session().clone();
        for part in s.verifier_ids() {
            let path = worktree::path_for(&s.main_worktree, &s.id, part);
            worktree::add(
                &s.main_worktree,
                &path,
                &target_dir,
                &s.manifest,
                &s.rebuild,
            )
            .unwrap_or_else(|e| panic!("{part}: {e}"));
            engine
                .handle(Input::WorktreeMade {
                    part: part.clone(),
                    path,
                })
                .unwrap();
        }
        let session = engine.session().clone();
        assert!(
            session
                .worktrees
                .values()
                .all(|t| t.state == WorktreeState::Made)
        );
        Fixture { root, dir, session }
    }

    fn tree(&self, part: &str) -> PathBuf {
        self.session.worktrees[&PartId::new(part)].path.clone()
    }

    fn target_dir(&self) -> PathBuf {
        store::target_dir(&store::reply_dir(&self.session))
    }

    fn check(&self) -> Result<(), String> {
        engine::check_resumable(&self.session, &self.dir)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn ac19_a_missing_worktree_is_no_refusal_and_is_rebuilt_from_the_recipe() {
    let f = Fixture::new();
    assert_eq!(f.check(), Ok(()));
    let codex = f.tree("codex");
    assert_eq!(
        fs::read_to_string(codex.join("src/lib.rs")).unwrap(),
        "pub fn one() -> u32 { 2 }\n"
    );
    fs::remove_dir_all(&codex).unwrap();
    assert_eq!(
        f.check(),
        Ok(()),
        "a missing worktree is rebuilt, not refused"
    );

    // The rebuild a resume makes: the same recipe, into the same place.
    let s = &f.session;
    worktree::add(
        &s.main_worktree,
        &codex,
        &f.target_dir(),
        &s.manifest,
        &s.rebuild,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        fs::read_to_string(codex.join("src/lib.rs")).unwrap(),
        "pub fn one() -> u32 { 2 }\n",
        "the tracked change is applied"
    );
    assert_eq!(
        fs::read_to_string(codex.join("src/new.rs")).unwrap(),
        "pub fn new() {}\n",
        "the untracked file is copied"
    );
    assert_eq!(
        worktree::check(&s.main_worktree, &codex, &s.rebuild.head_sha),
        Ok(())
    );
    assert_eq!(f.check(), Ok(()));
}

#[test]
fn ac19_a_worktree_repointed_to_a_branch_is_refused_naming_it() {
    let f = Fixture::new();
    git(&f.tree("claude"), &["checkout", "-q", "-b", "my-fix"]);
    let err = f.check().unwrap_err();
    assert!(
        err.starts_with("claude のワークツリーを再開できません"),
        "{err}"
    );
    assert!(err.contains("my-fix"), "{err}");
}

#[test]
fn ac19_a_worktree_at_another_commit_is_refused_naming_it() {
    let f = Fixture::new();
    let codex = f.tree("codex");
    git(&codex, &["checkout", "-q", "--detach", "HEAD~1"]);
    let before = git(&codex, &["rev-parse", "--short=7", "HEAD"]);
    let err = f.check().unwrap_err();
    assert!(
        err.starts_with("codex のワークツリーを再開できません"),
        "{err}"
    );
    assert!(err.contains(&before), "{err}");
}

#[test]
fn ac19_a_worktree_folder_that_is_not_a_worktree_is_refused() {
    let f = Fixture::new();
    let claude = f.tree("claude");
    // Removed from git, a plain folder left in its place.
    git(
        &f.session.main_worktree,
        &["worktree", "remove", "--force", claude.to_str().unwrap()],
    );
    fs::create_dir_all(&claude).unwrap();
    let err = f.check().unwrap_err();
    assert!(
        err.starts_with("claude のワークツリーを再開できません"),
        "{err}"
    );
}

#[test]
fn ac19_a_changed_snapshot_or_spec_copy_refuses_the_resume() {
    let f = Fixture::new();
    let spec = f.target_dir().join(capture::SPEC_FILE);
    let text = fs::read_to_string(&spec).unwrap();
    fs::write(&spec, "# Spec\n\n- one() returns 3\n").unwrap();
    let err = f.check().unwrap_err();
    assert!(err.contains("仕様ファイル"), "{err}");
    fs::write(&spec, text).unwrap();
    assert_eq!(f.check(), Ok(()));

    let patch = f.target_dir().join(capture::WORKTREE_PATCH);
    let bytes = fs::read(&patch).unwrap();
    fs::write(&patch, b"tampered\n").unwrap();
    let err = f.check().unwrap_err();
    assert!(err.contains("スナップショット"), "{err}");
    fs::write(&patch, bytes).unwrap();
    assert_eq!(f.check(), Ok(()));
}
