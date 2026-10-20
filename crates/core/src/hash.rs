use std::fmt;

use serde::{Deserialize, Serialize};

/// A 32 byte BLAKE3 hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    pub const ZERO: Hash = Hash([0; 32]);

    pub fn of(data: &[u8]) -> Hash {
        Hash(*blake3::hash(data).as_bytes())
    }

    pub fn from_blake3(h: blake3::Hash) -> Hash {
        Hash(*h.as_bytes())
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// First 12 hex characters, used for display.
    pub fn short(&self) -> String {
        hex::encode(&self.0[..6])
    }

    pub fn from_hex(s: &str) -> Option<Hash> {
        let v = hex::decode(s).ok()?;
        Self::from_slice(&v)
    }

    pub fn from_slice(v: &[u8]) -> Option<Hash> {
        let arr: [u8; 32] = v.try_into().ok()?;
        Some(Hash(arr))
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", self.short())
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let h = Hash::of(b"hello");
        assert_eq!(Hash::from_hex(&h.to_hex()), Some(h));
        assert_eq!(h.short().len(), 12);
        assert!(Hash::from_hex("zz").is_none());
    }
}
