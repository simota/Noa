//! The binary's command line: `--help`, flags it refuses before any
//! window, and `--snapshot` rendering every view headlessly (AC-25).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn verify(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_verify"))
        .args(args)
        .output()
        .unwrap()
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("noa-verify-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether `path` holds a PNG (its signature).
fn is_png(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
}

#[test]
fn help_prints_the_usage_and_succeeds() {
    let out = verify(&["--help"]);
    assert!(out.status.success());
    let usage = String::from_utf8_lossy(&out.stdout);
    for flag in [
        "--demo",
        "--demo-start",
        "--demo-held",
        "--demo-criteria",
        "--session=DIR",
        "--repo=DIR",
        "--tab=criteria|evidence|gallery|defects|report",
        "--criterion=ID",
        "--snapshot=FILE.png",
    ] {
        assert!(usage.contains(flag), "{flag}: {usage}");
    }
}

#[test]
fn bad_flags_are_refused_before_any_window() {
    for (args, says) in [
        (vec!["--what"], "unknown option"),
        (vec!["--tab=matrix"], "unknown tab"),
        (vec!["--demo", "--demo-held"], "do not go together"),
        (vec!["--size=0x10", "--demo"], "bad size"),
        (vec!["--repo=relative/dir"], "--repo"),
        (vec!["--repo=/tmp", "--demo-start"], "--repo"),
    ] {
        let out = verify(&args);
        assert!(!out.status.success(), "{args:?} was accepted");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(says), "{args:?}: {err}");
    }
}

#[test]
fn an_unreadable_log_exits_with_failure_and_writes_no_snapshot() {
    let dir = temp("bad-log");
    let log_dir = dir.join("ver-bad");
    std::fs::create_dir_all(&log_dir).unwrap();
    std::fs::write(log_dir.join("events.jsonl"), "not json\n").unwrap();
    let png = dir.join("out.png");
    for session in [log_dir.clone(), dir.join("ver-missing")] {
        let out = verify(&[
            &format!("--session={}", session.display()),
            &format!("--snapshot={}", png.display()),
        ]);
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
fn ac25_every_view_renders_to_a_png_headlessly() {
    let dir = temp("snapshots");
    let views: &[&[&str]] = &[
        &["--demo"],
        &["--demo-start"],
        &["--demo-held"],
        &["--demo-criteria"],
        &["--demo", "--tab=criteria"],
        &["--demo", "--tab=evidence"],
        &["--demo", "--tab=gallery"],
        &["--demo", "--tab=defects"],
        &["--demo", "--tab=report"],
        &["--demo", "--criterion=AC-1"],
        &["--demo-held", "--tab=gallery", "--criterion=AC-2"],
    ];
    for (n, view) in views.iter().enumerate() {
        let png = dir.join(format!("{n}.png"));
        let mut args: Vec<String> = view.iter().map(|a| (*a).to_owned()).collect();
        args.push(format!("--snapshot={}", png.display()));
        args.push("--size=1200x800".to_owned());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = verify(&args);
        assert!(
            out.status.success(),
            "{view:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(is_png(&png), "{view:?} wrote no PNG");
        // Each is only checked to exist; they are large.
        std::fs::remove_file(&png).unwrap();
    }
    let _ = std::fs::remove_dir_all(dir);
}
