//! Pure, I/O-free core of MacSync.
//!
//! Everything in here is deterministic and side-effect free so that it can be
//! covered by property-based tests: content hashes, entries, version vectors,
//! the per-unit Merkle tree, the conflict decision logic, the classification of
//! paths inside `.git` and the wire protocol messages.

pub mod codec;
pub mod decide;
pub mod entry;
pub mod gitpath;
pub mod hash;
pub mod merkle;
pub mod path;
pub mod proto;
pub mod vv;

pub use decide::{decide, ConflictClass, Decision, Policy, Prefer};
pub use entry::{Entry, Kind, UnitKind};
pub use gitpath::GitClass;
pub use hash::Hash;
pub use merkle::Tree;
pub use vv::{Ordering as VvOrdering, VersionVector};

/// Short, human readable identifier of a device (16 hex characters).
pub type DeviceId = String;

/// Protocol version spoken on the wire. Peers with a different major version
/// refuse to talk to each other.
pub const PROTOCOL_VERSION: u32 = 3;
