//! Offline previews produce only the requested PNG. They need neither a
//! running Noa nor a control token, and invalid data fails before drawing.

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.noa-widgets/test-summary/widget.json")
}

fn render(path: &Path) -> Vec<u8> {
    let result = Command::new(env!("CARGO_BIN_EXE_widget-host"))
        .arg(format!("--manifest={}", manifest().display()))
        .arg(format!("--snapshot={}", path.display()))
        .arg("--size=800x600")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    std::fs::read(path).unwrap()
}

#[test]
fn offline_snapshot_is_deterministic_and_bounded_to_the_requested_file() {
    let dir = tempfile::tempdir().unwrap();
    let first = render(&dir.path().join("first.png"));
    assert_eq!(&first[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(u32::from_be_bytes(first[16..20].try_into().unwrap()), 1600);
    assert_eq!(u32::from_be_bytes(first[20..24].try_into().unwrap()), 1200);
    let second = render(&dir.path().join("second.png"));
    assert_eq!(first, second);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn schema_invalid_data_never_creates_a_preview() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("invalid.json");
    let output = dir.path().join("preview.png");
    std::fs::write(&data, b"{}").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_widget-host"))
        .arg(format!("--manifest={}", manifest().display()))
        .arg(format!("--data={}", data.display()))
        .arg(format!("--snapshot={}", output.display()))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());
    assert!(String::from_utf8_lossy(&result.stderr).contains("required"));
}

#[test]
fn special_file_inputs_are_rejected_without_waiting_for_a_writer() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("link.json");
    let fifo = dir.path().join("fifo.json");
    symlink(manifest(), &link).unwrap();
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // A FIFO supplied as a manifest must fail without waiting for its writer.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for path in [link, fifo] {
        let output = dir.path().join("unexpected.png");
        let mut child = Command::new(env!("CARGO_BIN_EXE_widget-host"))
            .arg(format!("--manifest={}", path.display()))
            .arg(format!("--snapshot={}", output.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("widget-host blocked reading {}", path.display());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(!status.success());
        assert!(!output.exists());
    }
}
