//! FNV-1a 64, the hash behind candidate ids (I3) and the data folder's repo
//! key (I5), as in `widgets/tasks/src/store.rs`.

const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(OFFSET, |h, b| (h ^ u64::from(*b)).wrapping_mul(PRIME))
}

/// `fnv1a64` as 16 lowercase hex digits.
pub fn hex(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a64(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hex(b"foobar"), "85944171f73967e8");
    }
}
