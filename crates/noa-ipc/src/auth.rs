//! Scopes and token auth (spec FR-3, FR-5, FR-6, NFR-1).

use rand::RngCore;
use std::fs;
use std::io;
use std::path::Path;

/// One noa-ipc authorization scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Read,
    Control,
    Input,
    Attach,
}

impl Scope {
    fn bit(self) -> u8 {
        match self {
            Scope::Read => 1 << 0,
            Scope::Control => 1 << 1,
            Scope::Input => 1 << 2,
            Scope::Attach => 1 << 3,
        }
    }

    fn parse(s: &str) -> Option<Scope> {
        match s.trim() {
            "read" => Some(Scope::Read),
            "control" => Some(Scope::Control),
            "input" => Some(Scope::Input),
            "attach" => Some(Scope::Attach),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Control => "control",
            Scope::Input => "input",
            Scope::Attach => "attach",
        }
    }
}

/// A set of granted/configured/requested scopes, stored as a bitset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ScopeSet(u8);

impl ScopeSet {
    pub const fn empty() -> Self {
        ScopeSet(0)
    }

    /// The config default: `read` only (FR-6).
    pub fn default_read_only() -> Self {
        let mut s = ScopeSet::empty();
        s.insert(Scope::Read);
        s
    }

    pub fn insert(&mut self, scope: Scope) {
        self.0 |= scope.bit();
    }

    pub fn contains(self, scope: Scope) -> bool {
        self.0 & scope.bit() != 0
    }

    pub fn intersection(self, other: ScopeSet) -> ScopeSet {
        ScopeSet(self.0 & other.0)
    }

    /// Parses a comma-separated scope list (config `server-scopes` or a
    /// `noa.hello` request's `scopes` array). Unknown tokens are ignored.
    pub fn parse_list(s: &str) -> ScopeSet {
        let mut set = ScopeSet::empty();
        for token in s.split(',') {
            if let Some(scope) = Scope::parse(token) {
                set.insert(scope);
            }
        }
        set
    }

    pub fn from_strings<I, S>(items: I) -> ScopeSet
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut set = ScopeSet::empty();
        for item in items {
            if let Some(scope) = Scope::parse(item.as_ref()) {
                set.insert(scope);
            }
        }
        set
    }

    pub fn to_strings(self) -> Vec<String> {
        let mut out = Vec::new();
        for scope in [Scope::Read, Scope::Control, Scope::Input, Scope::Attach] {
            if self.contains(scope) {
                out.push(scope.as_str().to_string());
            }
        }
        out
    }
}

/// Constant-time byte comparison (no timing side-channel on token length
/// mismatch is out of scope; the equal-length path is constant-time).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Loads the server's bearer token, or provisions one on first run (FR-3).
///
/// - `configured` (config `server-token`) short-circuits: no file I/O — but
///   only when non-empty after trimming. A blank `server-token = ""` (or
///   whitespace-only) is treated as *absent*, not as a valid empty bearer
///   token that anyone could authenticate with (R-1): it falls through to
///   the file load/create path below, with a warning so a misconfigured
///   config doesn't silently drop the auth requirement.
/// - Otherwise reads `path`; if missing or empty, generates 32 random bytes
///   (hex-encoded) and writes them to `path` with `0600` permissions,
///   creating parent directories as needed.
pub fn load_or_create_token(path: &Path, configured: Option<&str>) -> io::Result<String> {
    if let Some(token) = configured {
        if !token.trim().is_empty() {
            return Ok(token.to_string());
        }
        log::warn!("noa-ipc: server-token is empty; falling back to generated token file");
    }
    if let Some(existing) = read_existing_token(path) {
        return Ok(existing);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Publish the freshly generated token with a create-if-absent link
    // rather than a truncating write: two noa processes initializing the
    // same config dir at once (B03, 2026-09 audit) must end up agreeing on
    // one token, whichever of them wins the `bind` later. The first to link
    // owns the file; the loser discards its candidate and adopts the
    // winner's. The file is never observable empty or half-written because
    // the bytes land in a private temp file before the link.
    let token = generate_token();
    match publish_token_file(path, &token) {
        Ok(()) => {
            repair_token_file_permissions(path);
            Ok(token)
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => read_existing_token(path)
            .ok_or_else(|| {
                io::Error::other("token file appeared during creation but could not be read")
            }),
        Err(err) => Err(err),
    }
}

/// The token in `path`, if the file exists and holds one. An existing
/// *empty* file is removed (R-1) so `publish_token_file` can create a fresh
/// 0600 file in its place; `OpenOptions::mode` only applies at creation.
fn read_existing_token(path: &Path) -> Option<String> {
    let existing = fs::read_to_string(path).ok()?;
    let trimmed = existing.trim();
    if trimmed.is_empty() {
        let _ = fs::remove_file(path);
        return None;
    }
    repair_token_file_permissions(path);
    Some(trimmed.to_string())
}

/// Repairs (not rejects) an existing token file's permissions on unix if
/// group/other bits leaked in (e.g. a restrictive `umask` wasn't in effect
/// when it was written, or it was copied from elsewhere) — F-4 / FR-3.
/// No-op on failure to `stat`/`chmod` beyond a warning; a read-only
/// filesystem or unusual ACL setup must not block startup.
#[cfg(unix)]
fn repair_token_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(err) => {
            log::warn!("noa-ipc: cannot stat token file {}: {err}", path.display());
            return;
        }
    };
    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 {
        log::warn!(
            "noa-ipc: token file {} has overly permissive mode {mode:o}, repairing to 0600",
            path.display()
        );
        if let Err(err) = fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            log::warn!("noa-ipc: failed to repair token file permissions: {err}");
        }
    }
}

#[cfg(not(unix))]
fn repair_token_file_permissions(_path: &Path) {}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes `token` to a private temp file next to `path` and links it into
/// place with a create-if-absent semantics: fails with `AlreadyExists` (temp
/// file removed) when another process published first.
fn publish_token_file(path: &Path, token: &str) -> io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let (tmp, mut file) = loop {
        let tmp = staging_path(path);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&tmp) {
            Ok(file) => break (tmp, file),
            // A previous process with a reused PID may have left this name
            // behind. Only a collision at the final link is a competing token.
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    };
    let result = (|| {
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
        fs::hard_link(&tmp, path)
    })();
    drop(file);
    let _ = fs::remove_file(&tmp);
    result
}

/// Per-process, per-thread-unique staging name so concurrent publishers
/// never share a temp file.
fn staging_path(path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "server-token".to_string());
    path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}
