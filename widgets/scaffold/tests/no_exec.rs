//! Nothing runs a process (Decision 2, AC5): the source holds no way to
//! start one, and scanning a repo seeded with traps that would run on any
//! `git` call or script execution leaves no marker.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn the_source_starts_no_process() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(
        files.len() > 10,
        "{} holds the crate's sources",
        src.display()
    );
    let forbidden = [
        "std::process",
        "process::Command",
        "Command::new",
        "libc::exec",
        "libc::fork",
        "libc::system",
        "libc::popen",
        "posix_spawn",
    ];
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for word in forbidden {
            assert!(!text.contains(word), "{} contains {word:?}", file.display());
        }
    }
}

fn put(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn executable(path: &Path, text: &str) {
    put(path, text);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn scanning_a_trapped_repo_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let marker = dir.path().join("marker");
    let trap = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
    put(
        &repo.join(".git/config"),
        &format!(
            "[core]\n\tfsmonitor = {}\n[alias]\n\tstatus = !touch '{}'\n",
            repo.join(".git/hooks/fsmonitor").display(),
            marker.display()
        ),
    );
    put(&repo.join(".git/HEAD"), "ref: refs/heads/main\n");
    put(
        &repo.join(".git/refs/heads/main"),
        "0123456789abcdef0123456789abcdef01234567\n",
    );
    for hook in [
        "fsmonitor",
        "pre-commit",
        "post-checkout",
        "post-index-change",
        "reference-transaction",
    ] {
        executable(&repo.join(".git/hooks").join(hook), &trap);
    }
    for script in [
        "scripts/verify.sh",
        "scripts/bundle-macos.sh",
        "scripts/package-linux.sh",
        "scripts/check-widget-registration.py",
    ] {
        executable(&repo.join(script), &trap);
    }
    put(
        &repo.join("dev/scaffold/registry.json"),
        "{\"schema\": 1, \"widgets\": {}, \"assets\": []}\n",
    );
    put(
        &repo.join("widgets/a/Cargo.toml"),
        "[package]\nname = \"a\"\n",
    );
    put(&repo.join("widgets/a/src/main.rs"), "fn main() {}\n");

    let report = scaffold::scan::scan(&repo);
    assert_eq!(report.exit, 2, "the trap scripts have no anchors");
    assert_eq!(
        scaffold::gitread::head_commit(&repo).as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    let _ = scaffold::gitread::repo_identity(&repo);
    let _ = scaffold::registry::load(&repo);
    let _ = scaffold::setup::scan(&repo);
    assert!(!marker.exists(), "a trap ran");
}
