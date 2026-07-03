//! Parser DFA states (the vt100.net / DEC ANSI state machine).
//!
//! DCS collection is represented by a compact payload state plus an ESC-after-
//! payload state for ST termination.

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
    SosPmApcString,
}
