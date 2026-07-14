//! Assembled control-sequence payloads.

use core::fmt;

pub const MAX_PARAMS: usize = 32;
pub const MAX_INTERMEDIATES: usize = 2;
const MAX_SEPARATORS: usize = MAX_PARAMS - 1;

/// A fully-parsed CSI (Control Sequence Introducer) sequence.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Csi {
    /// Numeric parameters (may be empty). Missing/`0` are resolved to the
    /// caller's default via [`Csi::param`].
    params: Params,
    /// `sep_colon[k]` is `true` when the separator *between* `params[k]` and
    /// `params[k+1]` was a colon (`:`), used to disambiguate SGR `38:2:...`.
    sep_colon: Separators,
    /// Intermediate bytes (`0x20..=0x2f`).
    intermediates: Intermediates,
    /// Private / prefix marker (`<`, `=`, `>`, `?`), or `0` if none.
    pub private: u8,
    /// The final dispatch byte (`0x40..=0x7e`).
    pub final_byte: u8,
}

impl Csi {
    pub fn new(
        params: &[u16],
        sep_colon: &[bool],
        intermediates: &[u8],
        private: u8,
        final_byte: u8,
    ) -> Self {
        assert!(params.len() <= MAX_PARAMS);
        assert!(sep_colon.len() <= params.len().saturating_sub(1));
        assert!(intermediates.len() <= MAX_INTERMEDIATES);
        Self {
            params: Params::from_slice(params),
            sep_colon: Separators::from_slice(sep_colon),
            intermediates: Intermediates::from_slice(intermediates),
            private,
            final_byte,
        }
    }

    pub(crate) fn from_parts(
        params: Params,
        sep_colon: Separators,
        intermediates: Intermediates,
        private: u8,
        final_byte: u8,
    ) -> Self {
        Self {
            params,
            sep_colon,
            intermediates,
            private,
            final_byte,
        }
    }

    pub fn params(&self) -> &[u16] {
        self.params.as_slice()
    }

    pub fn intermediates(&self) -> &[u8] {
        self.intermediates.as_slice()
    }

    /// Whether the separator between `params[idx]` and `params[idx + 1]` was `:`.
    pub fn separator_is_colon(&self, idx: usize) -> bool {
        self.sep_colon.is_colon(idx)
    }

    /// Parameter at `idx`, resolving missing-or-`0` to `default`.
    pub fn param(&self, idx: usize, default: u16) -> u16 {
        match self.params().get(idx) {
            None | Some(0) => default,
            Some(&v) => v,
        }
    }
}

impl fmt::Debug for Csi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Csi")
            .field("params", &self.params())
            .field("sep_colon", &self.sep_colon)
            .field("intermediates", &self.intermediates())
            .field("private", &self.private)
            .field("final_byte", &self.final_byte)
            .finish()
    }
}

/// A fully-parsed `ESC`-introduced sequence (non-CSI, non-OSC).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Esc {
    intermediates: Intermediates,
    pub final_byte: u8,
}

impl Esc {
    pub fn new(intermediates: &[u8], final_byte: u8) -> Self {
        assert!(intermediates.len() <= MAX_INTERMEDIATES);
        Self {
            intermediates: Intermediates::from_slice(intermediates),
            final_byte,
        }
    }

    pub(crate) fn from_parts(intermediates: Intermediates, final_byte: u8) -> Self {
        Self {
            intermediates,
            final_byte,
        }
    }

    pub fn intermediates(&self) -> &[u8] {
        self.intermediates.as_slice()
    }
}

impl fmt::Debug for Esc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Esc")
            .field("intermediates", &self.intermediates())
            .field("final_byte", &self.final_byte)
            .finish()
    }
}

/// A completed DCS payload (raw bytes between `ESC P` and ST).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DcsPayload {
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Params {
    values: [u16; MAX_PARAMS],
    len: u8,
}

impl Params {
    fn from_slice(values: &[u16]) -> Self {
        assert!(values.len() <= MAX_PARAMS);
        let mut out = Self::default();
        out.values[..values.len()].copy_from_slice(values);
        out.len = values.len() as u8;
        out
    }

    pub(crate) fn as_slice(&self) -> &[u16] {
        &self.values[..self.len as usize]
    }

    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn len(&self) -> usize {
        self.len as usize
    }

    pub(crate) fn push(&mut self, value: u16) {
        if self.len() < MAX_PARAMS {
            self.values[self.len()] = value;
            self.len += 1;
        }
    }

    pub(crate) fn last_mut(&mut self) -> Option<&mut u16> {
        if self.is_empty() {
            None
        } else {
            Some(&mut self.values[self.len() - 1])
        }
    }
}

impl PartialEq for Params {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for Params {}

impl fmt::Debug for Params {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Intermediates {
    bytes: [u8; MAX_INTERMEDIATES],
    len: u8,
}

impl Intermediates {
    fn from_slice(bytes: &[u8]) -> Self {
        assert!(bytes.len() <= MAX_INTERMEDIATES);
        let mut out = Self::default();
        out.bytes[..bytes.len()].copy_from_slice(bytes);
        out.len = bytes.len() as u8;
        out
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }

    pub(crate) fn len(&self) -> usize {
        self.len as usize
    }

    pub(crate) fn push(&mut self, byte: u8) {
        if self.len() < MAX_INTERMEDIATES {
            self.bytes[self.len()] = byte;
            self.len += 1;
        }
    }
}

impl PartialEq for Intermediates {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for Intermediates {}

impl fmt::Debug for Intermediates {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Separators {
    colon_bits: u32,
    len: u8,
}

impl Separators {
    fn from_slice(sep_colon: &[bool]) -> Self {
        assert!(sep_colon.len() <= MAX_SEPARATORS);
        let mut out = Self::default();
        for &colon in sep_colon {
            out.push(colon);
        }
        out
    }

    pub(crate) fn clear(&mut self) {
        self.colon_bits = 0;
        self.len = 0;
    }

    pub(crate) fn push(&mut self, colon: bool) {
        if self.len as usize >= MAX_SEPARATORS {
            return;
        }
        if colon {
            self.colon_bits |= 1u32 << self.len;
        }
        self.len += 1;
    }

    pub(crate) fn is_colon(&self, idx: usize) -> bool {
        idx < self.len as usize && (self.colon_bits & (1u32 << idx)) != 0
    }

    fn active_bits(&self) -> u32 {
        if self.len == 0 {
            0
        } else {
            self.colon_bits & ((1u32 << self.len) - 1)
        }
    }
}

impl PartialEq for Separators {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.active_bits() == other.active_bits()
    }
}

impl Eq for Separators {}

impl fmt::Debug for Separators {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries((0..self.len as usize).map(|idx| self.is_colon(idx)))
            .finish()
    }
}
