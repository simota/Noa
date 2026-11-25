//! `agent-usage --demo --snapshot` renders both tabs headlessly to PNGs at
//! twice the logical size; without `--demo` a snapshot is refused.

use std::path::Path;
use std::process::Command;

/// Width and height from a PNG's IHDR chunk.
fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(
        &bytes[..8],
        b"\x89PNG\r\n\x1a\n",
        "{} is not a PNG",
        path.display()
    );
    assert_eq!(&bytes[12..16], b"IHDR");
    let be = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

fn snapshot(args: &[&str], name: &str) -> (u32, u32) {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join(name);
    // A prefs file in the temp dir would show a read; none may appear.
    let out = Command::new(env!("CARGO_BIN_EXE_agent-usage"))
        .args(args)
        .arg(format!("--snapshot={}", png.display()))
        .env("HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let size = png_size(&png);
    assert!(
        !dir.path().join("data").exists(),
        "the snapshot wrote prefs"
    );
    size
}

#[test]
fn demo_limits_snapshot_renders() {
    assert_eq!(
        snapshot(&["--demo", "--tab=limits"], "limits.png"),
        (2560, 1720)
    );
    assert_eq!(
        snapshot(&["--demo", "--tab=limits", "--size=1000x700"], "small.png"),
        (2000, 1400)
    );
}

#[test]
fn demo_stats_snapshot_renders() {
    assert_eq!(
        snapshot(
            &["--demo", "--tab=stats", "--period=30d", "--agent=all"],
            "stats.png"
        ),
        (2560, 1720)
    );
}

#[test]
fn snapshot_without_demo_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("x.png");
    let out = Command::new(env!("CARGO_BIN_EXE_agent-usage"))
        .arg(format!("--snapshot={}", png.display()))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--snapshot needs --demo"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!png.exists());
}
