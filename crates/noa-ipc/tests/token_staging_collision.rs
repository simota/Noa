//! Kept in a separate test binary so the first provisioning attempt uses
//! staging sequence zero, independent of other token tests running in parallel.

use noa_ipc::load_or_create_token;

#[test]
fn stale_staging_files_do_not_block_token_provisioning() {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("noa-ipc-stale-staging-{pid}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server-token");
    let _ = std::fs::remove_file(&path);
    let stale_paths: Vec<_> = (0..3)
        .map(|seq| dir.join(format!(".server-token.{pid}.{seq}.tmp")))
        .collect();
    for stale in &stale_paths {
        std::fs::write(stale, "stale-candidate").unwrap();
    }

    let token = load_or_create_token(&path, None).unwrap();
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), token);
    assert_eq!(load_or_create_token(&path, None).unwrap(), token);
    for stale in &stale_paths {
        assert_eq!(std::fs::read_to_string(stale).unwrap(), "stale-candidate");
    }
    // token + 3 stale staging files + the advisory `server-token.lock`.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 5);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
