//! Entries: the synchronised description of a single file system object.

use serde::{Deserialize, Serialize};

use crate::hash::Hash;
use crate::vv::VersionVector;

/// Type of a file system object that MacSync can transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum Kind {
    File,
    Dir,
    Symlink,
}

impl Kind {
    pub fn as_i64(self) -> i64 {
        match self {
            Kind::File => 0,
            Kind::Dir => 1,
            Kind::Symlink => 2,
        }
    }

    pub fn from_i64(v: i64) -> Option<Kind> {
        match v {
            0 => Some(Kind::File),
            1 => Some(Kind::Dir),
            2 => Some(Kind::Symlink),
            _ => None,
        }
    }

    fn tag(self) -> u8 {
        self.as_i64() as u8
    }
}

/// Type of a sync unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitKind {
    /// A directory containing a `.git` directory or `.git` file.
    Git,
    /// Any other directory.
    Folder,
    /// The files directly inside the root directory (unit name `.`).
    Root,
}

impl UnitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UnitKind::Git => "git",
            UnitKind::Folder => "folder",
            UnitKind::Root => "root",
        }
    }

    pub fn parse(s: &str) -> Option<UnitKind> {
        match s {
            "git" => Some(UnitKind::Git),
            "folder" => Some(UnitKind::Folder),
            "root" => Some(UnitKind::Root),
            _ => None,
        }
    }
}

/// Name of the unit holding the loose files of the root directory.
pub const ROOT_UNIT: &str = ".";

/// One file, directory or symlink inside a unit, or a tombstone.
///
/// `path` is relative to the unit root, uses `/` as separator and is NFC
/// normalised. The unit root itself has the empty path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
    /// Permission bits (`0o7777`). Ignored for symlinks.
    pub mode: u32,
    pub size: u64,
    /// Modification time in nanoseconds since the epoch. Transferred but never
    /// part of any hash and never used for decisions.
    pub mtime_ns: i64,
    /// BLAKE3 of the raw bytes (files only). Used for transfer and verification.
    pub blob: Option<Hash>,
    /// Comparison hash (files only). Equal to `blob` except for files with a
    /// semantic hash such as `.git/index`.
    pub cmp: Option<Hash>,
    /// Raw symlink target (symlinks only).
    pub target: Option<Vec<u8>>,
    /// Extended attributes, sorted by name.
    pub xattrs: Vec<(String, Vec<u8>)>,
    /// BSD file flags (macOS). Zero elsewhere.
    pub flags: u32,
    /// For hard link groups: the path of the group leader inside the same unit.
    pub link: Option<String>,
    pub deleted: bool,
    pub vv: VersionVector,
    /// Earlier contents of this file, newest first (at most [`LINEAGE`]),
    /// carried along with the entry. With it either Mac can tell an old
    /// version that another tool wrote back (a replay) from a new change.
    /// Not part of the state hash.
    pub prev: Vec<Hash>,
}

/// How many earlier contents an entry remembers.
pub const LINEAGE: usize = 8;

impl Entry {
    pub fn tombstone(path: &str, kind: Kind, vv: VersionVector) -> Entry {
        Entry {
            path: path.to_string(),
            kind,
            mode: 0,
            size: 0,
            mtime_ns: 0,
            blob: None,
            cmp: None,
            target: None,
            xattrs: Vec::new(),
            flags: 0,
            link: None,
            deleted: true,
            vv,
            prev: Vec::new(),
        }
    }

    /// Tombstone for the deletion of `prev`. It keeps what was deleted (kind,
    /// content, attributes), so that a copy on the other Mac that is merely
    /// concurrent but identical goes as well. None of it is part of the state
    /// hash: all tombstones stay equal.
    pub fn deletion_of(prev: &Entry, vv: VersionVector) -> Entry {
        Entry { size: 0, mtime_ns: 0, deleted: true, vv, ..prev.clone() }
    }

    /// The content of a live file (what the state hash uses), else `None`.
    pub fn content(&self) -> Option<Hash> {
        (!self.deleted && self.kind == Kind::File).then(|| self.cmp.or(self.blob)).flatten()
    }

    /// The lineage of a new version that replaces `old`: `old`'s content in
    /// front of `old`'s own lineage.
    pub fn lineage_after(old: &Entry) -> Vec<Hash> {
        let mut out = Vec::with_capacity(LINEAGE);
        for h in old.content().iter().chain(old.prev.iter()) {
            if out.len() == LINEAGE {
                break;
            }
            if !out.contains(h) {
                out.push(*h);
            }
        }
        out
    }

    /// True if `other` is an old version of this file written back: its
    /// content is one this file had before, and it is not younger. A version
    /// that is newer on disk (`git reset`, a restore that stamps the time of
    /// restoring) is a real change, not a replay.
    pub fn replayed_by(&self, other: &Entry) -> bool {
        match (self.content(), other.content()) {
            (Some(mine), Some(theirs)) => {
                mine != theirs && self.prev.contains(&theirs) && other.mtime_ns <= self.mtime_ns
            }
            _ => false,
        }
    }

    /// For a tombstone: the state hash of what it deleted, if it knows.
    /// Tombstones written before 0.5.0 do not.
    pub fn deleted_state(&self) -> Option<Hash> {
        if !self.deleted {
            return None;
        }
        let known = match self.kind {
            Kind::File => self.cmp.or(self.blob).is_some(),
            Kind::Symlink => self.target.is_some(),
            Kind::Dir => self.mode != 0,
        };
        known.then(|| Entry { deleted: false, ..self.clone() }.state_hash())
    }

    /// Hash of everything that constitutes the *state* of this entry, excluding
    /// its name, timestamps and version vector. Two entries with the same state
    /// hash are interchangeable.
    pub fn state_hash(&self) -> Hash {
        let mut h = blake3::Hasher::new();
        h.update(b"reposync-entry-v1\0");
        if self.deleted {
            h.update(b"deleted");
            return Hash::from_blake3(h.finalize());
        }
        h.update(&[self.kind.tag()]);
        let mode = if self.kind == Kind::Symlink { 0 } else { self.mode & 0o7777 };
        h.update(&mode.to_le_bytes());
        h.update(&self.flags.to_le_bytes());
        match self.kind {
            Kind::File => {
                h.update(&self.cmp.or(self.blob).unwrap_or(Hash::ZERO).0);
            }
            Kind::Symlink => {
                let t = self.target.as_deref().unwrap_or_default();
                h.update(&(t.len() as u64).to_le_bytes());
                h.update(t);
            }
            Kind::Dir => {}
        }
        match &self.link {
            Some(l) => {
                h.update(&[1]);
                h.update(&(l.len() as u64).to_le_bytes());
                h.update(l.as_bytes());
            }
            None => {
                h.update(&[0]);
            }
        }
        h.update(&(self.xattrs.len() as u64).to_le_bytes());
        for (k, v) in &self.xattrs {
            h.update(&(k.len() as u64).to_le_bytes());
            h.update(k.as_bytes());
            h.update(&(v.len() as u64).to_le_bytes());
            h.update(v);
        }
        Hash::from_blake3(h.finalize())
    }

    /// True if both entries describe the same state (or are both deleted).
    pub fn same_state(&self, other: &Entry) -> bool {
        if self.deleted || other.deleted {
            return self.deleted == other.deleted;
        }
        self.kind == other.kind && self.state_hash() == other.state_hash()
    }

    /// True if the entry only differs in content-irrelevant metadata so that it
    /// can be updated in place (same kind, same bytes).
    pub fn same_content(&self, other: &Entry) -> bool {
        !self.deleted
            && !other.deleted
            && self.kind == other.kind
            && self.blob == other.blob
            && self.target == other.target
            && self.link == other.link
    }

    /// Name of the last path component.
    pub fn name(&self) -> &str {
        crate::path::file_name(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn file(path: &str, content: &[u8]) -> Entry {
        let h = Hash::of(content);
        Entry {
            path: path.into(),
            kind: Kind::File,
            mode: 0o644,
            size: content.len() as u64,
            mtime_ns: 1,
            blob: Some(h),
            cmp: Some(h),
            target: None,
            xattrs: vec![],
            flags: 0,
            link: None,
            deleted: false,
            vv: VersionVector::new(),
            prev: Vec::new(),
        }
    }

    #[test]
    fn lineage_keeps_earlier_contents_newest_first() {
        let v1 = file("f", b"1");
        let v2 = Entry { prev: Entry::lineage_after(&v1), ..file("f", b"2") };
        let v3 = Entry { prev: Entry::lineage_after(&v2), mtime_ns: 3, ..file("f", b"3") };
        assert_eq!(v3.prev, vec![Hash::of(b"2"), Hash::of(b"1")]);
        assert_eq!(v3.state_hash(), file("f", b"3").state_hash(), "lineage is not state");
        // An old version written back (not younger) is a replay; a newer one is not.
        assert!(v3.replayed_by(&Entry { mtime_ns: 2, ..file("f", b"1") }));
        assert!(!v3.replayed_by(&Entry { mtime_ns: 9, ..file("f", b"1") }));
        assert!(!v3.replayed_by(&Entry { mtime_ns: 1, ..file("f", b"other") }));
        let mut long = v3.clone();
        for i in 0..20u8 {
            long = Entry { prev: Entry::lineage_after(&long), ..file("f", &[i]) };
        }
        assert_eq!(long.prev.len(), LINEAGE);
    }

    #[test]
    fn state_hash_ignores_mtime_and_vv() {
        let a = file("a", b"x");
        let mut b = a.clone();
        b.mtime_ns = 99;
        b.vv.increment("A");
        b.path = "other".into();
        assert_eq!(a.state_hash(), b.state_hash());
    }

    #[test]
    fn state_hash_sees_mode_xattrs_content() {
        let a = file("a", b"x");
        let mut m = a.clone();
        m.mode = 0o755;
        assert_ne!(a.state_hash(), m.state_hash());
        let mut x = a.clone();
        x.xattrs.push(("user.tag".into(), b"red".to_vec()));
        assert_ne!(a.state_hash(), x.state_hash());
        assert_ne!(a.state_hash(), file("a", b"y").state_hash());
    }

    #[test]
    fn cmp_takes_precedence_over_blob() {
        let mut a = file(".git/index", b"bytes-1");
        let mut b = file(".git/index", b"bytes-2");
        let sem = Hash::of(b"semantic");
        a.cmp = Some(sem);
        b.cmp = Some(sem);
        assert!(a.same_state(&b));
        assert!(!a.same_content(&b));
    }

    #[test]
    fn deletion_remembers_what_it_deleted() {
        let mut a = file("a", b"x");
        a.vv.increment("A");
        let t = Entry::deletion_of(&a, VersionVector::single("A", 2));
        assert!(t.deleted && t.same_state(&Entry::tombstone("a", Kind::File, VersionVector::new())));
        assert_eq!(t.state_hash(), Entry::tombstone("a", Kind::Dir, VersionVector::new()).state_hash());
        assert_eq!(t.deleted_state(), Some(a.state_hash()));
        assert_eq!(Entry::tombstone("a", Kind::File, VersionVector::new()).deleted_state(), None);
        assert_eq!(a.deleted_state(), None);
    }

    #[test]
    fn tombstones_are_equal() {
        let t1 = Entry::tombstone("a", Kind::File, VersionVector::single("A", 1));
        let t2 = Entry::tombstone("a", Kind::Dir, VersionVector::single("B", 3));
        assert!(t1.same_state(&t2));
        assert!(!t1.same_state(&file("a", b"")));
    }
}
