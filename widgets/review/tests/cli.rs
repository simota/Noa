//! The binary's exit status (AC-26): a log that cannot be read is refused
//! before any window or snapshot.

use std::process::Command;

#[test]
fn ac26_an_unreadable_log_exits_with_failure_and_writes_no_snapshot() {
    let dir = std::env::temp_dir().join(format!("noa-review-cli-{}", std::process::id()));
    let log_dir = dir.join("rev-bad");
    std::fs::create_dir_all(&log_dir).unwrap();
    std::fs::write(log_dir.join("events.jsonl"), "not json\n").unwrap();
    let png = dir.join("out.png");
    for session in [log_dir.clone(), dir.join("rev-missing")] {
        let out = Command::new(env!("CARGO_BIN_EXE_review"))
            .arg(format!("--session={}", session.display()))
            .arg(format!("--snapshot={}", png.display()))
            .output()
            .unwrap();
        assert!(!out.status.success(), "{} was accepted", session.display());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("を開けません"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!png.exists());
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_relative_or_mixed_repo_is_refused_before_any_window() {
    for args in [
        vec!["--repo=relative/dir"],
        vec!["--repo=/tmp", "--demo-start"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_review"))
            .args(&args)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?} was accepted");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--repo"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
