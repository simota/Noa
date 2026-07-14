//! Attach lease registry and the lossless raw-output queue.
//!
//! `noa-app` supplies the authoritative terminal integration through
//! [`crate::backend::IpcBackend::open_attach`]. This module deliberately owns
//! no terminal state: the backend registers [`AttachOutputSender`] and builds
//! the synthetic seed under its own single terminal lock, then returns the
//! seed before the server performs any WebSocket writes.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use rand::RngCore;

use crate::auth::constant_time_eq;
use crate::backend::PaneRef;

/// One-time attach reservations expire quickly so an abandoned control RPC
/// cannot pin a pane indefinitely.
pub const ATTACH_TOKEN_TTL: Duration = Duration::from_secs(10);

/// A slow raw client may buffer at most 1 MiB of PTY output in userspace.
pub const ATTACH_OUTPUT_CAPACITY_BYTES: usize = 1024 * 1024;

/// Producers wait at most two seconds for a slow raw client before closing
/// the queue. Bytes already accepted into the queue remain ordered and may be
/// drained; no accepted byte is silently dropped.
pub const ATTACH_BACKPRESSURE_TIMEOUT: Duration = Duration::from_secs(2);

/// Payload bound for every raw attach Binary message. Keeping application
/// chunks well below both peers' 256 KiB WebSocket frame cap makes large
/// seeds, pastes, and PTY bursts independent of tungstenite fragmentation.
pub(crate) const ATTACH_BINARY_CHUNK_BYTES: usize = 64 * 1024;

/// Aggregate client-side seed cap. The 512 MiB ceiling covers the maximum
/// valid two-screen grid even when every cell carries full RGB/SGR state plus
/// the bounded grapheme tail, while still bounding a peer that never sends
/// the seed terminator.
pub(crate) const MAX_ATTACH_SEED_BYTES: usize = 512 * 1024 * 1024;

/// Generations must remain unique across `Server::start` cycles. A registry is
/// server-owned and therefore short-lived during config reload, while stale
/// connection guards may outlive that server and call into the same backend.
static NEXT_ATTACH_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LeaseIdentity {
    pub pane: PaneRef,
    pub generation: u64,
}

pub(crate) struct AttachReservation {
    pub token: String,
    pub identity: LeaseIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReserveError {
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthenticateError {
    Invalid,
    Expired,
}

enum LeasePhase {
    Reserved { token: String, expires_at: Instant },
    Active,
}

struct LeaseEntry {
    generation: u64,
    phase: LeasePhase,
}

#[derive(Default)]
struct RegistryState {
    panes: HashMap<PaneRef, LeaseEntry>,
    tokens: HashMap<String, PaneRef>,
}

/// Server-owned single-attach registry. Every mutation is generation-aware so
/// cleanup from an old socket cannot detach a newer connection for the pane.
#[derive(Clone, Default)]
pub(crate) struct AttachRegistry {
    state: Arc<Mutex<RegistryState>>,
}

impl AttachRegistry {
    pub fn reserve(&self, pane: PaneRef) -> Result<AttachReservation, ReserveError> {
        self.reserve_at(pane, Instant::now())
    }

    fn reserve_at(&self, pane: PaneRef, now: Instant) -> Result<AttachReservation, ReserveError> {
        let mut state = self.state.lock();
        cleanup_expired(&mut state, now);
        if state.panes.contains_key(&pane) {
            return Err(ReserveError::Conflict);
        }

        let generation = next_attach_generation();
        let token = loop {
            let candidate = random_token();
            if !state.tokens.contains_key(&candidate) {
                break candidate;
            }
        };
        state.tokens.insert(token.clone(), pane);
        state.panes.insert(
            pane,
            LeaseEntry {
                generation,
                phase: LeasePhase::Reserved {
                    token: token.clone(),
                    expires_at: now + ATTACH_TOKEN_TTL,
                },
            },
        );

        Ok(AttachReservation {
            token,
            identity: LeaseIdentity { pane, generation },
        })
    }

    pub fn authenticate(&self, presented: &[u8]) -> Result<LeaseIdentity, AuthenticateError> {
        self.authenticate_at(presented, Instant::now())
    }

    fn authenticate_at(
        &self,
        presented: &[u8],
        now: Instant,
    ) -> Result<LeaseIdentity, AuthenticateError> {
        let mut state = self.state.lock();

        // The raw route is intentionally fixed (`/attach`), so the first
        // Binary frame is the only place the one-time secret is accepted.
        // Scan every outstanding token without an early return; a matching
        // token must not be observable through a HashMap lookup timing
        // difference. Tokens are all fixed-length 64-byte hex strings.
        let mut matched = None;
        for (token, &pane) in &state.tokens {
            if constant_time_eq(presented, token.as_bytes()) {
                matched = Some((token.clone(), pane));
            }
        }
        let Some((token, pane)) = matched else {
            cleanup_expired(&mut state, now);
            return Err(AuthenticateError::Invalid);
        };
        let Some(entry) = state.panes.get(&pane) else {
            state.tokens.remove(&token);
            return Err(AuthenticateError::Invalid);
        };
        let (generation, expires_at) = match &entry.phase {
            LeasePhase::Reserved { expires_at, .. } => (entry.generation, *expires_at),
            LeasePhase::Active => return Err(AuthenticateError::Invalid),
        };

        if now >= expires_at {
            remove_pane(&mut state, pane);
            return Err(AuthenticateError::Expired);
        }

        // Remove the token before activating the lease. A replay on another
        // `/attach` connection therefore cannot authenticate, even while the
        // first raw connection is still active.
        state.tokens.remove(&token);
        if let Some(entry) = state.panes.get_mut(&pane) {
            entry.phase = LeasePhase::Active;
        }
        Ok(LeaseIdentity { pane, generation })
    }

    pub fn release_pane(&self, pane: PaneRef) -> Option<LeaseIdentity> {
        let mut state = self.state.lock();
        let generation = state.panes.get(&pane)?.generation;
        remove_pane(&mut state, pane);
        Some(LeaseIdentity { pane, generation })
    }

    pub fn release_generation(&self, identity: LeaseIdentity) -> bool {
        let mut state = self.state.lock();
        let matches = state
            .panes
            .get(&identity.pane)
            .is_some_and(|entry| entry.generation == identity.generation);
        if matches {
            remove_pane(&mut state, identity.pane);
        }
        matches
    }

    pub fn is_active(&self, identity: LeaseIdentity) -> bool {
        self.state
            .lock()
            .panes
            .get(&identity.pane)
            .is_some_and(|entry| {
                entry.generation == identity.generation && matches!(entry.phase, LeasePhase::Active)
            })
    }

    /// Runs `operation` only while `identity` owns the current reservation or
    /// active attach lease. The registry lock stays held through the operation
    /// so a concurrent raw disconnect cannot invalidate the lease between the
    /// ownership check and the protected action.
    pub fn with_current_lease<T>(
        &self,
        identity: LeaseIdentity,
        operation: impl FnOnce() -> T,
    ) -> Option<T> {
        let mut state = self.state.lock();
        cleanup_expired(&mut state, Instant::now());
        state
            .panes
            .get(&identity.pane)
            .is_some_and(|entry| entry.generation == identity.generation)
            .then(operation)
    }
}

fn next_attach_generation() -> u64 {
    let previous = NEXT_ATTACH_GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.wrapping_add(1).max(1))
        })
        .expect("the generation update closure always returns Some");
    previous.wrapping_add(1).max(1)
}

fn cleanup_expired(state: &mut RegistryState, now: Instant) {
    let expired: Vec<PaneRef> = state
        .panes
        .iter()
        .filter_map(|(&pane, entry)| match entry.phase {
            LeasePhase::Reserved { expires_at, .. } if now >= expires_at => Some(pane),
            _ => None,
        })
        .collect();
    for pane in expired {
        remove_pane(state, pane);
    }
}

fn remove_pane(state: &mut RegistryState, pane: PaneRef) {
    if let Some(entry) = state.panes.remove(&pane)
        && let LeasePhase::Reserved { token, .. } = entry.phase
    {
        state.tokens.remove(&token);
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(token, "{byte:02x}");
    }
    token
}

struct OutputState {
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    sender_count: usize,
    receiver_alive: bool,
    closed: bool,
}

struct OutputInner {
    state: Mutex<OutputState>,
    readable: Condvar,
    writable: Condvar,
}

/// Producer endpoint registered by the application raw tap. Cloning is
/// supported for implementations that need to move the sink between their IO
/// thread and pane registry without exposing the receiver.
pub struct AttachOutputSender {
    inner: Arc<OutputInner>,
}

impl Clone for AttachOutputSender {
    fn clone(&self) -> Self {
        self.inner.state.lock().sender_count += 1;
        AttachOutputSender {
            inner: self.inner.clone(),
        }
    }
}

impl AttachOutputSender {
    pub fn send(&self, bytes: Vec<u8>) -> Result<(), AttachOutputError> {
        self.send_timeout(bytes, ATTACH_BACKPRESSURE_TIMEOUT)
    }

    pub fn send_timeout(&self, bytes: Vec<u8>, timeout: Duration) -> Result<(), AttachOutputError> {
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > ATTACH_OUTPUT_CAPACITY_BYTES {
            self.close();
            return Err(AttachOutputError::CapacityExceeded);
        }

        let deadline = Instant::now() + timeout;
        let mut state = self.inner.state.lock();
        loop {
            if state.closed || !state.receiver_alive {
                return Err(AttachOutputError::Closed);
            }
            if state.queued_bytes + bytes.len() <= ATTACH_OUTPUT_CAPACITY_BYTES {
                state.queued_bytes += bytes.len();
                state.queue.push_back(bytes);
                self.inner.readable.notify_one();
                return Ok(());
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero()
                || self
                    .inner
                    .writable
                    .wait_for(&mut state, remaining)
                    .timed_out()
            {
                state.closed = true;
                self.inner.readable.notify_all();
                self.inner.writable.notify_all();
                return Err(AttachOutputError::TimedOut);
            }
        }
    }

    pub fn close(&self) {
        let mut state = self.inner.state.lock();
        state.closed = true;
        self.inner.readable.notify_all();
        self.inner.writable.notify_all();
    }
}

impl Drop for AttachOutputSender {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock();
        state.sender_count = state.sender_count.saturating_sub(1);
        if state.sender_count == 0 {
            state.closed = true;
            self.inner.readable.notify_all();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AttachOutputError {
    #[error("attach output receiver closed")]
    Closed,
    #[error("attach output exceeded the 1 MiB capacity")]
    CapacityExceeded,
    #[error("attach output remained backpressured until its deadline")]
    TimedOut,
}

pub(crate) struct AttachOutputReceiver {
    inner: Arc<OutputInner>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttachTryRecvError {
    Empty,
    Closed,
}

impl AttachOutputReceiver {
    pub fn try_recv(&self) -> Result<Vec<u8>, AttachTryRecvError> {
        let mut state = self.inner.state.lock();
        if let Some(bytes) = state.queue.pop_front() {
            state.queued_bytes -= bytes.len();
            self.inner.writable.notify_all();
            return Ok(bytes);
        }
        if state.closed || state.sender_count == 0 {
            Err(AttachTryRecvError::Closed)
        } else {
            Err(AttachTryRecvError::Empty)
        }
    }
}

impl Drop for AttachOutputReceiver {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock();
        state.receiver_alive = false;
        state.closed = true;
        self.inner.writable.notify_all();
    }
}

pub(crate) fn output_channel() -> (AttachOutputSender, AttachOutputReceiver) {
    let inner = Arc::new(OutputInner {
        state: Mutex::new(OutputState {
            queue: VecDeque::new(),
            queued_bytes: 0,
            sender_count: 1,
            receiver_alive: true,
            closed: false,
        }),
        readable: Condvar::new(),
        writable: Condvar::new(),
    });
    (
        AttachOutputSender {
            inner: inner.clone(),
        },
        AttachOutputReceiver { inner },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_enforces_single_attach_and_expiry_without_sleep() {
        let registry = AttachRegistry::default();
        let now = Instant::now();
        let first = registry.reserve_at(7, now).unwrap();
        assert!(matches!(
            registry.reserve_at(7, now),
            Err(ReserveError::Conflict)
        ));

        assert_eq!(first.token.len(), 64);
        assert!(first.token.bytes().all(|byte| byte.is_ascii_hexdigit()));

        // The lease expires at the deadline, not one tick after it.
        assert_eq!(
            registry.authenticate_at(first.token.as_bytes(), now + ATTACH_TOKEN_TTL),
            Err(AuthenticateError::Expired)
        );
        let second = registry.reserve_at(7, now + ATTACH_TOKEN_TTL).unwrap();
        assert_ne!(first.token, second.token);
    }

    #[test]
    fn current_lease_operation_rejects_expired_and_released_generations() {
        let registry = AttachRegistry::default();
        let current = registry.reserve(7).unwrap();
        assert_eq!(
            registry.with_current_lease(current.identity, || "owned"),
            Some("owned")
        );

        assert!(registry.release_generation(current.identity));
        assert_eq!(
            registry.with_current_lease(current.identity, || "stale"),
            None
        );

        let expired = registry
            .reserve_at(8, Instant::now() - ATTACH_TOKEN_TTL)
            .unwrap();
        assert_eq!(
            registry.with_current_lease(expired.identity, || "expired"),
            None
        );
    }

    #[test]
    fn token_is_one_time_and_mismatch_does_not_identify_or_release_a_pane() {
        let registry = AttachRegistry::default();
        let reservation = registry.reserve(9).unwrap();
        assert_eq!(
            registry.authenticate(b"wrong"),
            Err(AuthenticateError::Invalid)
        );
        assert!(matches!(registry.reserve(9), Err(ReserveError::Conflict)));
        let identity = registry.authenticate(reservation.token.as_bytes()).unwrap();
        assert_eq!(
            registry.authenticate(reservation.token.as_bytes()),
            Err(AuthenticateError::Invalid)
        );
        assert!(registry.is_active(identity));
    }

    #[test]
    fn stale_generation_cleanup_cannot_release_a_new_lease() {
        let registry = AttachRegistry::default();
        let first = registry.reserve(3).unwrap();
        let first_id = registry.authenticate(first.token.as_bytes()).unwrap();
        assert!(registry.release_generation(first_id));

        let second = registry.reserve(3).unwrap();
        let second_id = registry.authenticate(second.token.as_bytes()).unwrap();
        assert!(!registry.release_generation(first_id));
        assert!(registry.is_active(second_id));
    }

    #[test]
    fn byte_capacity_is_exact_and_timeout_closes_the_queue() {
        let (sender, receiver) = output_channel();
        sender
            .send_timeout(vec![1; ATTACH_OUTPUT_CAPACITY_BYTES], Duration::ZERO)
            .unwrap();
        assert_eq!(
            sender.send_timeout(vec![2], Duration::ZERO),
            Err(AttachOutputError::TimedOut)
        );
        assert_eq!(
            receiver.try_recv().unwrap().len(),
            ATTACH_OUTPUT_CAPACITY_BYTES
        );
        assert_eq!(receiver.try_recv(), Err(AttachTryRecvError::Closed));
    }

    #[test]
    fn separate_registries_issue_distinct_process_generations() {
        let first = AttachRegistry::default();
        let second = AttachRegistry::default();
        let first_reservation = first.reserve(7).unwrap();
        let second_reservation = second.reserve(7).unwrap();

        let first_identity = first
            .authenticate(first_reservation.token.as_bytes())
            .unwrap();
        let second_identity = second
            .authenticate(second_reservation.token.as_bytes())
            .unwrap();

        assert_ne!(first_identity.generation, second_identity.generation);
    }
}
