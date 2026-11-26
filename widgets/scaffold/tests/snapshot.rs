//! `scaffold --demo --snapshot` renders the demo headlessly to a PNG at
//! twice the logical size, the same bytes each run, and writes nothing
//! else; without `--demo` a snapshot is refused.

use std::path::Path;
use std::process::Command;

/// Width and height from a PNG's IHDR chunk.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    assert_eq!(&bytes[12..16], b"IHDR");
    let be = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

fn snapshot(dir: &Path, args: &[&str], name: &str) -> Vec<u8> {
    let png = dir.join(name);
    let out = Command::new(env!("CARGO_BIN_EXE_scaffold"))
        .args(args)
        .arg(format!("--snapshot={}", png.display()))
        .env("HOME", dir)
        .env("XDG_DATA_HOME", dir.join("data"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read(&png).unwrap()
}

#[test]
fn the_demo_snapshot_renders_at_twice_the_size_and_repeats() {
    let dir = tempfile::tempdir().unwrap();
    let a = snapshot(dir.path(), &["--demo"], "a.png");
    assert_eq!(png_size(&a), (2560, 1720));
    let b = snapshot(dir.path(), &["--demo"], "b.png");
    assert!(a == b, "two demo snapshots differ");
    let small = snapshot(dir.path(), &["--demo", "--size=1000x700"], "small.png");
    assert_eq!(png_size(&small), (2000, 1400));
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["a.png", "b.png", "small.png"],
        "the snapshot wrote something else"
    );
}

#[test]
fn a_snapshot_without_the_demo_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("x.png");
    let out = Command::new(env!("CARGO_BIN_EXE_scaffold"))
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
