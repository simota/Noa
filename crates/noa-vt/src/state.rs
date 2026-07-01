//! Parser DFA states (the vt100.net / DEC ANSI state machine).
//!
//! Inc-1 implements the ground/escape/CSI path fully; DCS sub-states are
//! collapsed to a single consuming `DcsPassthrough` (DCS carries no inc-1
//! side effects — full DCS handling lands in inc≥5).

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
    OscString,
    SosPmApcString,
}
