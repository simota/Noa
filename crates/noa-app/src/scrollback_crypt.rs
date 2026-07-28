//! Encryption for persisted scrollback snapshots (`scrollback-persist-encrypt`).
//!
//! Spec: `docs/specs/scrollback-persistence.md` §4.6 Stage 2.
//!
//! ## Why this lives here and not in `noa-grid`
//!
//! `noa-grid` is the platform-agnostic state model; it has no business knowing
//! about keychains. The snapshot format it produces is treated here as an
//! opaque plaintext and wrapped in its own container, so the two layers stay
//! independent: a snapshot decodes the same whether or not it was ever sealed,
//! and turning encryption on or off does not change the inner format at all.
//!
//! ## Container
//!
//! ```text
//! magic  6  b"NOAEN\0"
//! version 2 u16
//! nonce  12 random per write
//! body   …  AES-256-GCM(plaintext, aad = magic || version)
//! ```
//!
//! The AAD binds the header so a file cannot be replayed under a different
//! version. Nothing about the plaintext leaks except its length.
//!
//! ## Key
//!
//! One 256-bit key per user, generated on first use and stored as a generic
//! password in the login keychain, marked non-syncable so it never reaches
//! iCloud. Losing the keychain entry means losing the records — which is the
//! honest trade for at-rest protection, and the reason this is opt-in.

const MAGIC: &[u8; 6] = b"NOAEN\0";
const VERSION: u16 = 1;
const NONCE_LEN: usize = 12;
const HEADER_LEN: usize = 8;
const KEY_LEN: usize = 32;

#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "noa";
#[cfg(target_os = "macos")]
const KEYCHAIN_ACCOUNT: &str = "scrollback-persist-key";

/// Whether `bytes` is a sealed container (rather than a bare snapshot).
///
/// Read paths use this instead of the config value: a file written before
/// encryption was turned on is still readable afterwards, and one written
/// before it was turned off does not become garbage.
pub fn is_sealed(bytes: &[u8]) -> bool {
    bytes.len() >= HEADER_LEN && &bytes[..MAGIC.len()] == MAGIC
}

/// Fetch the snapshot key, creating and storing one on first use.
///
/// `None` when the keychain is unavailable or refuses — callers must then
/// decline to write rather than fall back to plaintext, since the user asked
/// for encryption specifically.
#[cfg(target_os = "macos")]
fn key() -> Option<[u8; KEY_LEN]> {
    use security_framework::passwords::{
        get_generic_password, set_generic_password_options,
    };
    use security_framework::passwords_options::PasswordOptions;

    if let Ok(existing) = get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
        && let Ok(key) = <[u8; KEY_LEN]>::try_from(existing.as_slice())
    {
        return Some(key);
    }

    let mut fresh = [0u8; KEY_LEN];
    security_framework::random::SecRandom::default()
        .copy_bytes(&mut fresh)
        .ok()?;

    let mut options = PasswordOptions::new_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);
    // Never sync to iCloud Keychain: a record of one machine's terminal output
    // has no business appearing on another.
    options.set_access_synchronized(Some(false));
    options.set_label("noa persisted scrollback key");
    match set_generic_password_options(&fresh, options) {
        Ok(()) => Some(fresh),
        Err(err) => {
            log::warn!("could not store the scrollback encryption key: {err}");
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn key() -> Option<[u8; KEY_LEN]> {
    None
}

/// Wrap `plaintext` in a sealed container. `None` when no key is available;
/// the caller must then skip the write rather than store it in the clear.
pub fn seal(plaintext: &[u8]) -> Option<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    random_bytes(&mut nonce)?;
    seal_with(&key()?, plaintext, &nonce)
}

/// The container half of [`seal`], with the key and nonce supplied.
///
/// Split out so the format is testable without touching a keychain: a unit
/// test that called [`seal`] would mint and store a real key on the developer's
/// machine as a side effect.
fn seal_with(
    key: &[u8; KEY_LEN],
    plaintext: &[u8],
    nonce_bytes: &[u8; NONCE_LEN],
) -> Option<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};

    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let nonce = Nonce::try_from(&nonce_bytes[..]).ok()?;

    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&VERSION.to_le_bytes());

    let sealed = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: &header,
            },
        )
        .ok()?;

    let mut out = Vec::with_capacity(HEADER_LEN + NONCE_LEN + sealed.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(nonce_bytes);
    out.extend_from_slice(&sealed);
    Some(out)
}

/// Unwrap a sealed container. `None` for anything malformed, truncated,
/// tampered with, or encrypted under a key this machine no longer has — the
/// same "degrade to no record" contract the snapshot format itself follows.
pub fn open(bytes: &[u8]) -> Option<Vec<u8>> {
    if !is_sealed(bytes) {
        return None;
    }
    open_with(&key()?, bytes)
}

/// The container half of [`open`], with the key supplied (see [`seal_with`]).
fn open_with(key: &[u8; KEY_LEN], bytes: &[u8]) -> Option<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};

    if !is_sealed(bytes) {
        return None;
    }
    let version = u16::from_le_bytes(bytes.get(6..8)?.try_into().ok()?);
    if version != VERSION {
        return None;
    }
    let nonce_bytes = bytes.get(HEADER_LEN..HEADER_LEN + NONCE_LEN)?;
    let ciphertext = bytes.get(HEADER_LEN + NONCE_LEN..)?;

    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    cipher
        .decrypt(
            &Nonce::try_from(nonce_bytes).ok()?,
            Payload {
                msg: ciphertext,
                aad: &bytes[..HEADER_LEN],
            },
        )
        .ok()
}

#[cfg(target_os = "macos")]
fn random_bytes(buf: &mut [u8]) -> Option<()> {
    security_framework::random::SecRandom::default()
        .copy_bytes(buf)
        .ok()
}

#[cfg(not(target_os = "macos"))]
fn random_bytes(_buf: &mut [u8]) -> Option<()> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_snapshot_is_not_mistaken_for_a_sealed_one() {
        // Read paths branch on the container, not on the config value, so a
        // file written before encryption was enabled must not be fed to the
        // decryptor.
        assert!(!is_sealed(b"NOASB\0\x01\x00"));
        assert!(!is_sealed(b""));
        assert!(!is_sealed(b"NOAE"));
        assert!(is_sealed(b"NOAEN\0\x01\x00"));
    }

    /// Tests never call [`seal`]/[`open`] directly: those mint and store a real
    /// key in the developer's login keychain as a side effect. The container is
    /// exercised through the `*_with` split instead.
    const TEST_KEY: [u8; KEY_LEN] = [7u8; KEY_LEN];

    fn seal_for_test(plaintext: &[u8]) -> Vec<u8> {
        seal_with(&TEST_KEY, plaintext, &[3u8; NONCE_LEN]).expect("sealing with a supplied key")
    }

    #[test]
    fn a_sealed_container_roundtrips_and_hides_its_plaintext() {
        let sealed = seal_for_test(b"secret output");

        assert!(is_sealed(&sealed));
        assert!(
            !sealed.windows(13).any(|w| w == b"secret output"),
            "the plaintext must not appear in the container"
        );
        assert_eq!(
            open_with(&TEST_KEY, &sealed).as_deref(),
            Some(&b"secret output"[..])
        );
    }

    #[test]
    fn a_tampered_container_fails_closed() {
        let sealed = seal_for_test(b"secret output");

        let mut body = sealed.clone();
        let last = body.len() - 1;
        body[last] ^= 0xff;
        assert!(
            open_with(&TEST_KEY, &body).is_none(),
            "GCM must reject a modified body"
        );

        // The header is authenticated as AAD, so editing it must fail too.
        let mut version = sealed.clone();
        version[6] = 9;
        assert!(open_with(&TEST_KEY, &version).is_none());

        let mut nonce = sealed.clone();
        nonce[HEADER_LEN] ^= 0xff;
        assert!(open_with(&TEST_KEY, &nonce).is_none());

        assert!(open_with(&TEST_KEY, &sealed[..HEADER_LEN + 4]).is_none());
    }

    #[test]
    fn the_wrong_key_cannot_open_a_container() {
        let sealed = seal_for_test(b"secret output");
        assert!(open_with(&[9u8; KEY_LEN], &sealed).is_none());
    }

    #[test]
    fn opening_a_bare_snapshot_returns_none() {
        assert!(open(b"NOASB\0\x01\x00whatever").is_none());
    }
}
