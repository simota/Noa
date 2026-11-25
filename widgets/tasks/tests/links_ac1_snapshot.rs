//! Task links AC-1 (`docs/specs/task-links.md`): `tasks --demo
//! --select=t-3 --snapshot` renders the demo task whose 関連成果 hold the
//! L0 picture to a PNG. Only the PNG's size is asserted, as
//! `tests/snapshot.rs` does.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!(
            "noa-tasks-links-snapshot-{name}-{}",
            std::process::id()
        ));
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

#[test]
fn links_ac1_the_demo_tasks_related_section_snapshots_to_a_png() {
    let dir = TempDir::new("t3");
    let cache = TempDir::new("t3-cache");
    let png = dir.0.join("links.png");
    let out = Command::new(env!("CARGO_BIN_EXE_tasks"))
        .args(["--demo", "--select=t-3", "--size=1280x860"])
        .arg(format!("--snapshot={}", png.display()))
        .env("HOME", &dir.0)
        .env("XDG_DATA_HOME", dir.0.join("data"))
        .env("XDG_CACHE_HOME", &cache.0)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(png_size(&png), (2560, 1720));
}
