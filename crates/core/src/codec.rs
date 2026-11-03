//! Binary encoding used on the wire, in the transaction journal and for
//! compact database columns. One place so that the format can be versioned
//! and replaced as a whole (currently `postcard`, a maintained serde format).

use serde::de::DeserializeOwned;
use serde::Serialize;

pub use postcard::Error;

pub fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, Error> {
    postcard::to_allocvec(value)
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    postcard::from_bytes(bytes)
}

/// Like [`decode`], but every byte must be used: tells two layouts of the
/// same data apart (a value of an older layout rarely fits exactly).
pub fn decode_exact<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    match postcard::take_from_bytes(bytes)? {
        (v, []) => Ok(v),
        _ => Err(Error::DeserializeBadEncoding),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Entry, Hash, Kind, VersionVector};

    #[test]
    fn entry_roundtrip() {
        let e = Entry {
            path: "a/ü.txt".into(),
            kind: Kind::File,
            mode: 0o644,
            size: 3,
            mtime_ns: -5,
            blob: Some(Hash::of(b"abc")),
            cmp: None,
            target: Some(vec![0, 255]),
            xattrs: vec![("user.x".into(), vec![1, 2, 3])],
            flags: 2,
            link: Some("b".into()),
            deleted: false,
            vv: VersionVector::single("dev", 7),
            prev: Vec::new(),
        };
        let b = encode(&e).unwrap();
        assert_eq!(decode::<Entry>(&b).unwrap(), e);
        assert!(decode::<Entry>(&b[..b.len() - 1]).is_err());
    }
}
