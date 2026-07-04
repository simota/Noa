//! Parser DFA states (the vt100.net / DEC ANSI state machine).
//!
//! DCS collection is represented by a compact payload state plus an ESC-after-
//! payload state for ST termination. APC (`ESC _`) uses the same two-state
//! shape so its bounded capture can reach a dispatch (Kitty graphics).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsPassthrough,
    DcsEscape,
    OscString,
    /// SOS (`ESC X`) / PM (`ESC ^`) payloads — captured by no one, discarded.
    SosPmApcString,
    /// APC (`ESC _`) payload — bounded capture toward [`crate::Action::ApcDispatch`].
    ApcString,
    /// ESC seen inside an APC payload; `\` finishes it (7-bit ST).
    ApcEscape,
}
