//! Token provisioning tests (spec AC-3).

use noa_ipc::load_or_create_token;

#[test]
fn generates_0600_nonempty_file_and_reuses_it() {
    let dir = std::env::temp_dir().join(format!("noa-ipc-token-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server-token");
    let _ = std::fs::remove_file(&path);

    let first = load_or_create_token(&path, None).unwrap();
    assert!(!first.is_empty());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    let second = load_or_create_token(&path, None).unwrap();
    assert_eq!(
        first, second,
        "second call must reuse the file, not regenerate"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---- F-4: repair, don't reject, overly permissive existing token files ----

#[cfg(unix)]
#[test]
fn repairs_overly_permissive_existing_token_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir =
        std::env::temp_dir().join(format!("noa-ipc-token-test-repair-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server-token");
    std::fs::write(&path, "existing-token").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let token = load_or_create_token(&path, None).unwrap();
    assert_eq!(
        token, "existing-token",
        "an existing token is reused, not rejected"
    );

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "loading an existing token repairs its permissions"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---- R-1: an empty existing token file must not keep lax permissions ----

#[cfg(unix)]
#[test]
fn regenerating_into_an_empty_0644_token_file_repairs_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!(
        "noa-ipc-token-test-empty-perms-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server-token");
    // An empty file, deliberately left at a lax mode (e.g. as if written by
    // a process with a permissive umask, or copied from elsewhere) —
    // `write_token_file`'s `OpenOptions::mode(0o600)` only applies at
    // *creation*, so truncating and rewriting this existing file would
    // otherwise leave the freshly generated secret sitting in a
    // group/other-readable file.
    std::fs::write(&path, "").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let token = load_or_create_token(&path, None).unwrap();
    assert!(
        !token.is_empty(),
        "an empty existing file must still trigger regeneration"
    );

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "the regenerated token file must not keep the old lax mode"
    );

    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        on_disk.trim(),
        token,
        "the file must contain the returned token"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn configured_token_skips_file_io() {
    let dir = std::env::temp_dir().join(format!(
        "noa-ipc-token-test-configured-{}",
        std::process::id()
    ));
    // deliberately do not create `dir` — if the function touched the
    // filesystem for the configured path, this would fail.
    let path = dir.join("server-token");

    let token = load_or_create_token(&path, Some("explicit-token")).unwrap();
    assert_eq!(token, "explicit-token");
    assert!(!path.exists());
}

// ---- R-1: an empty/blank configured token is treated as absent ----

#[test]
fn empty_configured_token_falls_back_to_generated_file_token() {
    let dir = std::env::temp_dir().join(format!("noa-ipc-token-test-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server-token");
    let _ = std::fs::remove_file(&path);

    let token = load_or_create_token(&path, Some("")).unwrap();
    assert!(
        !token.is_empty(),
        "an empty configured token must never become the live token"
    );
    assert!(
        path.exists(),
        "an empty configured token must fall through to the file provisioning path"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn whitespace_only_configured_token_falls_back_to_generated_file_token() {
    let dir = std::env::temp_dir().join(format!("noa-ipc-token-test-blank-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server-token");
    let _ = std::fs::remove_file(&path);

    let token = load_or_create_token(&path, Some("   \t")).unwrap();
    assert!(!token.is_empty());
    assert!(path.exists());

    std::fs::remove_dir_all(&dir).ok();
}
