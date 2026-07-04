//! Secure Keyboard Entry: while enabled, macOS routes key events only to this
//! process, so other apps (keyloggers, screen readers, other event taps) can't
//! observe typed keystrokes. Mirrors Terminal.app / Ghostty's "Secure Keyboard
//! Entry" toggle.
//!
//! macOS's secure event input is a *process-global, reference-counted* switch:
//! every `EnableSecureEventInput` must be balanced by a `DisableSecureEventInput`,
//! and while any process holds it enabled, key input to *every other* app is
//! blocked. To avoid holding it while backgrounded, the effective state is
//! `desired && app_focused` — enabled only while the user asked for it *and*
//! this app is frontmost (exactly Terminal.app's behavior). On exit the switch
//! is always released so the process never leaves it held.
//!
//! [`SecureInput`] is the pure state machine (unit-tested through a mock
//! [`SecureInputBackend`]); [`CarbonSecureInput`] is the real macOS backend. It
//! keeps `desired`/`active` in lockstep and issues exactly one enable per
//! disable, so the OS refcount can never leak or go negative.

/// The platform side effect, abstracted so the [`SecureInput`] state machine is
/// unit-testable without touching the real Carbon API.
pub(crate) trait SecureInputBackend {
    fn set_enabled(&mut self, enabled: bool);
}

/// Reconciles the user's intent and app focus into balanced enable/disable
/// calls. See the module docs for the `desired && app_focused` rule.
pub(crate) struct SecureInput {
    /// The user's intent, toggled via the menu / command palette. Persists
    /// across focus changes so re-focusing restores it.
    desired: bool,
    /// Whether *we* currently hold the OS switch on. The single source of truth
    /// that keeps enable/disable balanced.
    active: bool,
}

impl SecureInput {
    pub(crate) fn new() -> Self {
        Self {
            desired: false,
            active: false,
        }
    }

    /// Flip the user intent and reconcile against current focus. Returns the
    /// new desired state (drives the menu checkmark).
    pub(crate) fn toggle(
        &mut self,
        app_focused: bool,
        backend: &mut impl SecureInputBackend,
    ) -> bool {
        self.desired = !self.desired;
        self.reconcile(app_focused, backend);
        self.desired
    }

    /// Re-evaluate the effective state after the app gains or loses focus.
    pub(crate) fn on_focus_change(
        &mut self,
        app_focused: bool,
        backend: &mut impl SecureInputBackend,
    ) {
        self.reconcile(app_focused, backend);
    }

    /// Release the OS switch on app exit regardless of intent, so the process
    /// never leaves secure input held for the rest of the system.
    pub(crate) fn disable_for_exit(&mut self, backend: &mut impl SecureInputBackend) {
        if self.active {
            backend.set_enabled(false);
            self.active = false;
        }
    }

    fn reconcile(&mut self, app_focused: bool, backend: &mut impl SecureInputBackend) {
        let want = self.desired && app_focused;
        if want != self.active {
            backend.set_enabled(want);
            self.active = want;
        }
    }
}

/// The real macOS backend, calling Carbon's secure-event-input API. A no-op on
/// other platforms (the state machine still runs, just without side effects).
pub(crate) struct CarbonSecureInput;

impl SecureInputBackend for CarbonSecureInput {
    fn set_enabled(&mut self, enabled: bool) {
        #[cfg(target_os = "macos")]
        // SAFETY: both are parameterless Carbon calls with no invariants beyond
        // balanced refcounting, which `SecureInput` guarantees by issuing
        // exactly one enable per disable.
        unsafe {
            if enabled {
                macos::EnableSecureEventInput();
            } else {
                macos::DisableSecureEventInput();
            }
            log::debug!(
                "secure event input -> {enabled} (os now reports {})",
                macos::IsSecureEventInputEnabled() != 0
            );
        }
        let _ = enabled;
    }
}

#[cfg(target_os = "macos")]
mod macos {
    // Carbon (HIToolbox) secure-event-input API. `Enable`/`Disable` return an
    // `OSStatus` we don't need; `IsSecureEventInputEnabled` returns a C
    // `Boolean` (`unsigned char`), taken as `u8` and compared against zero.
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn EnableSecureEventInput() -> i32;
        pub(super) fn DisableSecureEventInput() -> i32;
        pub(super) fn IsSecureEventInputEnabled() -> u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records every backend call so tests can assert the exact enable/disable
    /// sequence.
    #[derive(Default)]
    struct MockBackend {
        calls: Vec<bool>,
    }

    impl SecureInputBackend for MockBackend {
        fn set_enabled(&mut self, enabled: bool) {
            self.calls.push(enabled);
        }
    }

    #[test]
    fn toggle_while_focused_enables_then_disables() {
        let mut state = SecureInput::new();
        let mut backend = MockBackend::default();

        assert!(state.toggle(true, &mut backend));
        assert!(!state.toggle(true, &mut backend));

        assert_eq!(backend.calls, vec![true, false]);
    }

    #[test]
    fn toggling_on_while_unfocused_does_not_enable_until_focus_returns() {
        let mut state = SecureInput::new();
        let mut backend = MockBackend::default();

        // Enabled by intent, but the app is backgrounded: no OS call yet.
        assert!(state.toggle(false, &mut backend));
        assert!(backend.calls.is_empty());

        // Focus returns -> enable; focus lost -> disable; back -> enable again.
        state.on_focus_change(true, &mut backend);
        state.on_focus_change(false, &mut backend);
        state.on_focus_change(true, &mut backend);

        assert_eq!(backend.calls, vec![true, false, true]);
    }

    #[test]
    fn focus_changes_while_disabled_never_touch_the_backend() {
        let mut state = SecureInput::new();
        let mut backend = MockBackend::default();

        state.on_focus_change(true, &mut backend);
        state.on_focus_change(false, &mut backend);
        state.on_focus_change(true, &mut backend);

        assert!(backend.calls.is_empty());
    }

    #[test]
    fn redundant_focus_events_are_coalesced() {
        let mut state = SecureInput::new();
        let mut backend = MockBackend::default();

        state.toggle(true, &mut backend); // enable
        state.on_focus_change(true, &mut backend); // already enabled: no-op
        state.on_focus_change(false, &mut backend); // disable
        state.on_focus_change(false, &mut backend); // already disabled: no-op

        assert_eq!(backend.calls, vec![true, false]);
    }

    #[test]
    fn exit_releases_a_held_switch_exactly_once() {
        let mut state = SecureInput::new();
        let mut backend = MockBackend::default();

        state.toggle(true, &mut backend); // enable
        state.disable_for_exit(&mut backend); // release
        state.disable_for_exit(&mut backend); // already released: no-op

        assert_eq!(backend.calls, vec![true, false]);
    }

    #[test]
    fn exit_while_unfocused_is_a_no_op_because_nothing_is_held() {
        let mut state = SecureInput::new();
        let mut backend = MockBackend::default();

        state.toggle(false, &mut backend); // desired but not active
        state.disable_for_exit(&mut backend);

        assert!(backend.calls.is_empty());
    }
}
