//! Assembled control-sequence payloads.

pub const MAX_PARAMS: usize = 32;
pub const MAX_INTERMEDIATES: usize = 2;

/// A fully-parsed CSI (Control Sequence Introducer) sequence.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Csi {
    /// Numeric parameters (may be empty). Missing/`0` are resolved to the
    /// caller's default via [`Csi::param`].
    pub params: Vec<u16>,
    /// `sep_colon[k]` is `true` when the separator *between* `params[k]` and
    /// `params[k+1]` was a colon (`:`), used to disambiguate SGR `38:2:...`.
    pub sep_colon: Vec<bool>,
    /// Intermediate bytes (`0x20..=0x2f`).
    pub intermediates: Vec<u8>,
    /// Private / prefix marker (`<`, `=`, `>`, `?`), or `0` if none.
    pub private: u8,
    /// The final dispatch byte (`0x40..=0x7e`).
    pub final_byte: u8,
}

impl Csi {
    /// Parameter at `idx`, resolving missing-or-`0` to `default`.
    pub fn param(&self, idx: usize, default: u16) -> u16 {
        match self.params.get(idx) {
            None | Some(0) => default,
            Some(&v) => v,
        }
    }
}

/// A fully-parsed `ESC`-introduced sequence (non-CSI, non-OSC).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Esc {
    pub intermediates: Vec<u8>,
    pub final_byte: u8,
}
