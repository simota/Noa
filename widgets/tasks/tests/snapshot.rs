//! AC-7: `tasks --demo --snapshot` renders the demo headlessly to a PNG at
//! twice the logical size and writes nothing else; without `--demo` a
//! snapshot is refused. Only the PNG's size is asserted (L2-3).

use std::path::{Path, PathBuf};
use std::process::Command;

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let path =
            std::env::temp_dir().join(format!("noa-tasks-snapshot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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
    let dir = TempDir::new(name);
    // The GPU driver's shader cache goes to XDG_CACHE_HOME (Mesa): kept
    // out of HOME, since it is not the widget's.
    let cache = TempDir::new(&format!("{name}-cache"));
    let png = dir.0.join(format!("{name}.png"));
    let out = Command::new(env!("CARGO_BIN_EXE_tasks"))
        .args(args)
        .arg(format!("--snapshot={}", png.display()))
        .env("HOME", &dir.0)
        .env("XDG_DATA_HOME", dir.0.join("data"))
        .env("XDG_CACHE_HOME", &cache.0)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let size = png_size(&png);
    // The PNG, and nothing else under HOME or XDG_DATA_HOME.
    let written: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(written, [png.file_name().unwrap()], "{args:?}");
    size
}

#[test]
fn ac07_demo_snapshot_renders_at_twice_the_size() {
    assert_eq!(snapshot(&["--demo"], "default"), (2560, 1720));
}

#[test]
fn ac07_demo_snapshot_takes_a_size_and_a_selected_card() {
    assert_eq!(
        snapshot(&["--demo", "--size=1200x800", "--select=#12"], "select"),
        (2400, 1600)
    );
    assert_eq!(snapshot(&["--demo", "--select=t-1"], "local"), (2560, 1720));
}

#[test]
fn every_layout_renders_with_a_selected_card() {
    for layout in ["list", "timeline", "calendar"] {
        assert_eq!(
            snapshot(
                &["--demo", &format!("--layout={layout}"), "--select=#15"],
                layout
            ),
            (2560, 1720),
            "{layout}"
        );
    }
}

#[test]
fn ac07_snapshot_without_demo_is_refused() {
    let dir = TempDir::new("refused");
    let png = dir.0.join("x.png");
    let out = Command::new(env!("CARGO_BIN_EXE_tasks"))
        .arg(format!("--snapshot={}", png.display()))
        .env("HOME", &dir.0)
        .env("XDG_DATA_HOME", dir.0.join("data"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--snapshot needs --demo"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!png.exists());
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
}
