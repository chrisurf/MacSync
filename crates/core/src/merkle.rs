//! Per-unit Merkle tree.
//!
//! Every directory node hash is BLAKE3 over its sorted children
//! `(name, kind, child hash)`. A file's leaf hash is its [`Entry::state_hash`];
//! a directory's hash additionally covers its own state (mode, xattrs). The
//! root hash summarises the complete unit in 32 bytes: equal roots on both
//! sides prove identical state. Tombstones are not part of the tree.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::entry::{Entry, Kind};
use crate::hash::Hash;
use crate::path;

/// A child of a directory node, as exchanged during Merkle descent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Child {
    pub name: String,
    pub kind: Kind,
    pub hash: Hash,
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    /// Directory path → sorted children.
    dirs: HashMap<String, Vec<Child>>,
    /// Path → node hash (files: leaf hash, dirs: subtree hash).
    hashes: HashMap<String, Hash>,
    root: Hash,
}

/// Hash of an empty unit (no live entries at all).
pub fn empty_root() -> Hash {
    Hash::of(b"reposync-empty-unit")
}

impl Tree {
    /// Build the tree from the live entries of one unit. Entries whose parent
    /// directory is missing are attached to their nearest existing ancestor so
    /// that the tree is always well formed.
    pub fn build<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> Tree {
        let mut live: BTreeMap<&str, &Entry> = BTreeMap::new();
        for e in entries {
            if !e.deleted {
                live.insert(e.path.as_str(), e);
            }
        }
        if live.is_empty() {
            return Tree { root: empty_root(), ..Default::default() };
        }

        // Group children by parent directory.
        let mut children: HashMap<String, Vec<&Entry>> = HashMap::new();
        for (&p, &e) in &live {
            if p.is_empty() {
                continue;
            }
            let mut parent = path::parent(p).unwrap_or("");
            while !parent.is_empty() && !live.get(parent).is_some_and(|d| d.kind == Kind::Dir) {
                parent = path::parent(parent).unwrap_or("");
            }
            children.entry(parent.to_string()).or_default().push(e);
        }

        let mut tree = Tree::default();
        // Process deepest paths first so that child hashes are known.
        let mut paths: Vec<&str> = live.keys().copied().collect();
        paths.sort_by_key(|p| std::cmp::Reverse(path::depth(p)));
        if !live.contains_key("") {
            paths.push("");
        }
        for p in paths {
            let own = live.get(p).copied();
            let is_dir = p.is_empty() || own.is_some_and(|e| e.kind == Kind::Dir);
            let hash = if is_dir {
                let mut kids: Vec<Child> = children
                    .get(p)
                    .map(|v| {
                        v.iter()
                            .map(|c| Child {
                                name: c.path.strip_prefix(p).unwrap_or(&c.path).trim_start_matches('/').to_string(),
                                kind: c.kind,
                                hash: tree.hashes[c.path.as_str()],
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                kids.sort_by(|a, b| a.name.cmp(&b.name));
                let mut h = blake3::Hasher::new();
                h.update(b"reposync-dir-v1\0");
                h.update(&own.map(|e| e.state_hash()).unwrap_or(Hash::ZERO).0);
                for c in &kids {
                    h.update(&(c.name.len() as u64).to_le_bytes());
                    h.update(c.name.as_bytes());
                    h.update(&[c.kind.as_i64() as u8]);
                    h.update(&c.hash.0);
                }
                tree.dirs.insert(p.to_string(), kids);
                Hash::from_blake3(h.finalize())
            } else {
                own.map(|e| e.state_hash()).unwrap_or(Hash::ZERO)
            };
            tree.hashes.insert(p.to_string(), hash);
        }
        tree.root = tree.hashes[""];
        tree
    }

    pub fn root(&self) -> Hash {
        self.root
    }

    /// Children of a directory, or `None` if `dir` is not a directory node.
    pub fn children(&self, dir: &str) -> Option<&[Child]> {
        self.dirs.get(dir).map(|v| v.as_slice())
    }

    pub fn hash_of(&self, path: &str) -> Option<Hash> {
        self.hashes.get(path).copied()
    }

    pub fn node_count(&self) -> usize {
        self.hashes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vv::VersionVector;
    use proptest::prelude::*;

    fn e(path: &str, kind: Kind, content: &str) -> Entry {
        let h = Hash::of(content.as_bytes());
        Entry {
            path: path.into(),
            kind,
            mode: if kind == Kind::Dir { 0o755 } else { 0o644 },
            size: content.len() as u64,
            mtime_ns: 0,
            blob: (kind == Kind::File).then_some(h),
            cmp: (kind == Kind::File).then_some(h),
            target: (kind == Kind::Symlink).then(|| content.as_bytes().to_vec()),
            xattrs: vec![],
            flags: 0,
            link: None,
            deleted: false,
            vv: VersionVector::new(),
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            e("", Kind::Dir, ""),
            e("src", Kind::Dir, ""),
            e("src/main.rs", Kind::File, "fn main(){}"),
            e("README", Kind::File, "hi"),
            e("link", Kind::Symlink, "README"),
        ]
    }

    #[test]
    fn root_changes_with_any_leaf() {
        let base = Tree::build(&sample());
        let mut changed = sample();
        changed[2] = e("src/main.rs", Kind::File, "fn main(){ }");
        let t2 = Tree::build(&changed);
        assert_ne!(base.root(), t2.root());
        assert_ne!(base.hash_of("src"), t2.hash_of("src"));
        assert_eq!(base.hash_of("README"), t2.hash_of("README"));
    }

    #[test]
    fn tombstones_do_not_count() {
        let mut s = sample();
        let base = Tree::build(&s).root();
        s.push(Entry::tombstone("gone", Kind::File, VersionVector::single("A", 1)));
        assert_eq!(Tree::build(&s).root(), base);
    }

    #[test]
    fn empty_dir_is_visible() {
        let mut s = sample();
        let base = Tree::build(&s).root();
        s.push(e("empty", Kind::Dir, ""));
        assert_ne!(Tree::build(&s).root(), base);
    }

    #[test]
    fn children_are_sorted() {
        let t = Tree::build(&sample());
        let names: Vec<_> = t.children("").unwrap().iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, vec!["README", "link", "src"]);
        assert!(t.children("README").is_none());
    }

    #[test]
    fn empty_unit() {
        assert_eq!(Tree::build(&[]).root(), empty_root());
    }

    proptest! {
        #[test]
        fn order_independent(perm in Just(sample()).prop_shuffle()) {
            prop_assert_eq!(Tree::build(&perm).root(), Tree::build(&sample()).root());
        }
    }
}
