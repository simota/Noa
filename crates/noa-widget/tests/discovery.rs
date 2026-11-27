use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use noa_widget::discovery::{discover, load, repo_context};
use serde_json::{Value, json};

struct Repository(PathBuf);

impl Repository {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "noa-widget-core-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.test",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ]);
        repo
    }

    fn git(&self, args: &[&str]) {
        let mut command = Command::new("git");
        command.arg("-C").arg(&self.0).args(args);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn definition(&self, id: &str) -> PathBuf {
        definition_at(&self.0, id)
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn definition_at(repo: &Path, id: &str) -> PathBuf {
    let mut value: Value = serde_json::from_slice(include_bytes!(
        "../../../.noa-widgets/test-summary/widget.json"
    ))
    .unwrap();
    value["id"] = json!(id);
    let path = repo.join(".noa-widgets").join(id).join("widget.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    path
}

#[test]
fn discovery_is_sorted_read_only_and_scoped_to_the_worktree() {
    let repo = Repository::new();
    assert!(discover(&repo.0).unwrap().is_empty());
    repo.definition("z-last");
    repo.definition("a-first");
    fs::create_dir(repo.0.join("subdir")).unwrap();
    let found = discover(&repo.0.join("subdir")).unwrap();
    assert_eq!(
        found
            .iter()
            .map(|d| d.manifest.id.as_str())
            .collect::<Vec<_>>(),
        ["a-first", "z-last"]
    );
    assert_eq!(found[0].digest, found[0].manifest.digest());
    assert_eq!(load(&repo.0, "a-first").unwrap().digest, found[0].digest);
    assert!(!repo.0.join(".noa").exists());
    assert!(load(&repo.0, "../escape").is_err());
}

#[test]
fn different_worktrees_share_repository_identity_but_not_definitions_or_state_identity() {
    let repo = Repository::new();
    repo.definition("main-only");
    let worktree = repo.0.join("linked");
    repo.git(&[
        "worktree",
        "add",
        "-qb",
        "other",
        worktree.to_str().unwrap(),
    ]);
    definition_at(&worktree, "worktree-only");
    let main = repo_context(&repo.0).unwrap();
    let linked = repo_context(&worktree).unwrap();
    assert_eq!(main.repository_id, linked.repository_id);
    assert_ne!(main.worktree_id, linked.worktree_id);
    assert_eq!(main.git_common_dir, linked.git_common_dir);
    assert_eq!(discover(&worktree).unwrap()[0].manifest.id, "worktree-only");
    assert!(load(&worktree, "main-only").is_err());
    let other_clone = Repository::new();
    assert_ne!(
        main.repository_id,
        repo_context(&other_clone.0).unwrap().repository_id
    );
}

#[test]
fn manifest_id_must_match_the_directory_and_invalid_files_are_reported() {
    let repo = Repository::new();
    let path = repo.definition("valid");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["id"] = json!("different");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(
        load(&repo.0, "valid")
            .unwrap_err()
            .to_string()
            .contains("must match")
    );
    fs::write(&path, b"not json").unwrap();
    assert!(discover(&repo.0).is_err());
    fs::write(&path, vec![b' '; noa_widget::MAX_MANIFEST_BYTES + 1]).unwrap();
    assert!(
        load(&repo.0, "valid")
            .unwrap_err()
            .to_string()
            .contains("64 KiB")
    );
}

#[test]
fn links_are_rejected_at_registry_package_and_file_boundaries() {
    use std::os::unix::fs::symlink;
    let repo = Repository::new();
    let external = Repository::new();
    let outside = external.definition("outside");
    symlink(external.0.join(".noa-widgets"), repo.0.join(".noa-widgets")).unwrap();
    assert!(discover(&repo.0).is_err());
    assert!(load(&repo.0, "outside").is_err());
    fs::remove_file(repo.0.join(".noa-widgets")).unwrap();
    fs::create_dir(repo.0.join(".noa-widgets")).unwrap();
    symlink(
        outside.parent().unwrap(),
        repo.0.join(".noa-widgets/outside"),
    )
    .unwrap();
    assert!(discover(&repo.0).is_err());
    assert!(load(&repo.0, "outside").is_err());
    fs::remove_file(repo.0.join(".noa-widgets/outside")).unwrap();
    fs::create_dir(repo.0.join(".noa-widgets/outside")).unwrap();
    symlink(&outside, repo.0.join(".noa-widgets/outside/widget.json")).unwrap();
    assert!(load(&repo.0, "outside").is_err());
}

#[test]
fn fifo_manifests_fail_without_waiting_for_a_writer() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let repo = Repository::new();
    let path = repo.definition("fifo");
    fs::remove_file(&path).unwrap();
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: c_path is a terminated path; mkfifo does not retain it.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert!(
        load(&repo.0, "fifo")
            .unwrap_err()
            .to_string()
            .contains("regular file")
    );
}

#[test]
fn packages_with_assets_are_loaded_and_pinned_by_their_bytes() {
    let repo = Repository::new();
    let package = repo.0.join(".noa-widgets/image-gallery");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.noa-widgets/image-gallery");
    fs::create_dir_all(package.join("assets")).unwrap();
    fs::copy(source.join("widget.json"), package.join("widget.json")).unwrap();
    for name in ["logo.png", "landscape.jpg", "pattern.webp"] {
        fs::copy(
            source.join("assets").join(name),
            package.join("assets").join(name),
        )
        .unwrap();
    }
    let first = load(&repo.0, "image-gallery").unwrap();
    assert_eq!(first.assets.len(), 3);
    assert_ne!(first.digest, first.manifest.digest());
    assert_eq!(discover(&repo.0).unwrap()[0].digest, first.digest);
    // Assets are never serialized with a described definition.
    assert!(!serde_json::to_string(&first).unwrap().contains("\"bytes\""));
    let logo = package.join("assets/logo.png");
    let mut bytes = fs::read(&logo).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&logo, bytes).unwrap();
    assert_ne!(load(&repo.0, "image-gallery").unwrap().digest, first.digest);
    fs::remove_file(&logo).unwrap();
    assert!(
        discover(&repo.0)
            .unwrap_err()
            .to_string()
            .contains("logo.png")
    );
    std::os::unix::fs::symlink(source.join("assets/logo.png"), &logo).unwrap();
    assert!(
        load(&repo.0, "image-gallery")
            .unwrap_err()
            .to_string()
            .contains("symbolic link")
    );
}
