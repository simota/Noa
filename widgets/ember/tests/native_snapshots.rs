//! Offline entry points must render without touching a user's store or provider.

#![cfg(feature = "native")]

use std::path::Path;
use std::process::Command;

fn dimensions(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    (
        u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
        u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
    )
}

#[test]
fn every_demo_renders_at_minimum_size_without_creating_a_store() {
    for kind in [
        "welcome",
        "conversation",
        "requests",
        "memory",
        "settings",
        "error",
    ] {
        let home = tempfile::tempdir().unwrap();
        let png = home.path().join("preview.png");
        let output = Command::new(env!("CARGO_BIN_EXE_ember"))
            .arg(format!("--demo={kind}"))
            .arg("--size=720x640")
            .arg(format!("--snapshot={}", png.display()))
            .env("HOME", home.path())
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env_remove("NOA_WIDGET_APPEARANCE_FILE")
            .env_remove("OPENAI_API_KEY")
            .env_remove("NOA_EMBER_OPENAI_API_KEY")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{kind}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(dimensions(&png), (1440, 1280));
        assert_eq!(
            std::fs::read_dir(home.path()).unwrap().count(),
            1,
            "{kind} wrote more than its PNG"
        );
    }
}

#[test]
fn snapshot_without_a_demo_fails_before_creating_any_user_files() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ember"))
        .arg(format!(
            "--snapshot={}",
            home.path().join("forbidden.png").display()
        ))
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join("data"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--snapshot requires --demo"));
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}
