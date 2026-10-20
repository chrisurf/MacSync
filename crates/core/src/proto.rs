//! Wire protocol messages exchanged between two daemons.
//!
//! The protocol is receiver driven: the side that wants to catch up asks for
//! roots, descends the Merkle tree, fetches entries for differing paths and
//! finally requests the content blobs it does not have locally.

use serde::{Deserialize, Serialize};

use crate::entry::{Entry, UnitKind};
use crate::hash::Hash;
use crate::merkle::Child;

/// Summary of one unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitRoot {
    pub name: String,
    pub kind: UnitKind,
    pub root: Hash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirListing {
    pub dir: String,
    /// `None` if `dir` is not a directory on the serving side.
    pub children: Option<Vec<Child>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Hello { device: String, name: String, protocol: u32, arch: String, root: String },
    Roots,
    Tree { unit: String, dirs: Vec<String> },
    /// Entries (including tombstones) for exact `paths` and for everything
    /// below each of `prefixes`.
    Entries { unit: String, paths: Vec<String>, prefixes: Vec<String> },
    /// Content of small blobs. Missing or changed blobs are returned as `None`.
    Blobs { hashes: Vec<Hash> },
    /// A byte range of a large blob.
    BlobRange { hash: Hash, offset: u64, len: u64 },
    /// Notification: these units changed on the sender.
    Changed { units: Vec<String> },
    Ping,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Hello { device: String, name: String, protocol: u32, arch: String, root: String },
    Roots(Vec<UnitRoot>),
    Tree(Vec<DirListing>),
    Entries(Vec<Entry>),
    Blobs(Vec<Option<Vec<u8>>>),
    /// `None` if the blob is no longer available in that version.
    BlobRange(Option<Vec<u8>>),
    Ok,
    Pong,
    Error(String),
}

/// Limits used by both sides.
pub mod limits {
    /// Files up to this size are bundled into `Blobs` requests.
    pub const SMALL_FILE: u64 = 256 * 1024;
    /// Target size of a bundle of small files.
    pub const BUNDLE_BYTES: u64 = 8 * 1024 * 1024;
    /// Maximum number of blobs in one bundle.
    pub const BUNDLE_COUNT: usize = 4096;
    /// Chunk size for large files.
    pub const RANGE_CHUNK: u64 = 4 * 1024 * 1024;
    /// Maximum number of directories per `Tree` request.
    pub const TREE_BATCH: usize = 512;
    /// Maximum number of paths per `Entries` request.
    pub const ENTRY_BATCH: usize = 4096;
    /// Hard limit for a single logical message.
    pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;
}
