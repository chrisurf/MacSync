//! A faulty or compromised peer must not make MacSync write outside the sync
//! root: no paths below symbolic links, no entries that were not asked for.

mod common;

use std::fs;
use std::path::Path;

use anyhow::Result;
use common::*;
use macsync_core::proto::{DirListing, UnitRoot};
use macsync_core::{Entry, Hash};
use macsync_engine::{Engine, PeerApi, PullOptions};

/// Serves `inner` but tampers with entry answers: `swap` replaces the entry
/// of the same path; `extra` is added to prefix requests that cover it, and
/// with `to_exact` to every exact-path request.
struct EvilPeer<'a> {
    inner: &'a Engine,
    swap: Option<Entry>,
    extra: Vec<Entry>,
    to_exact: bool,
}

impl PeerApi for EvilPeer<'_> {
    fn roots(&self) -> Result<Vec<UnitRoot>> {
        self.inner.roots()
    }
    fn tree(&self, unit: &str, dirs: &[String]) -> Result<Vec<DirListing>> {
        self.inner.serve_tree(unit, dirs)
    }
    fn entries(&self, unit: &str, paths: &[String], prefixes: &[String]) -> Result<Vec<Entry>> {
        let mut out = self.inner.entries(unit, paths, prefixes)?;
        if let Some(s) = &self.swap {
            for e in out.iter_mut().filter(|e| e.path == s.path) {
                *e = s.clone();
            }
        }
        for e in &self.extra {
            let covered = prefixes.iter().any(|p| macsync_core::path::is_within(&e.path, p));
            if covered || (self.to_exact && !paths.is_empty()) {
                out.push(e.clone());
            }
        }
        Ok(out)
    }
    fn blobs(&self, hashes: &[Hash]) -> Result<Vec<Option<Vec<u8>>>> {
        self.inner.blobs(hashes)
    }
    fn blob_range(&self, hash: &Hash, offset: u64, len: u64) -> Result<Option<Vec<u8>>> {
        self.inner.blob_range(hash, offset, len)
    }
}

/// The stored entry of `path` in unit `u` on `e`, renamed to `as_path`.
fn entry_as(e: &Engine, path: &str, as_path: &str) -> Entry {
    let mut entry = e.store().entry("u", path).unwrap().expect("entry").entry;
    entry.path = as_path.to_string();
    entry
}

fn warnings(e: &Engine, path: &str) -> usize {
    e.store().journal(0, Some("u"), 1000).unwrap().iter().filter(|l| l.action == "warning" && l.path == path).count()
}

fn empty(dir: &Path) -> bool {
    fs::read_dir(dir).unwrap().next().is_none()
}

fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// The peer turns folder `x` into `x → <outside>` but still sends `x/datei`
/// in the same answer: nothing appears outside.
#[test]
fn entries_below_a_delivered_symlink_are_dropped() {
    let p = Pair::new();
    let outside = tempfile::tempdir().unwrap();
    write(&p.ra(), "u/x/datei", "payload");
    std::os::unix::fs::symlink(outside.path(), p.ra().join("u/link")).unwrap();
    p.a.scan_all(false).unwrap();
    let evil = EvilPeer { inner: &p.a, swap: Some(entry_as(&p.a, "link", "x")), extra: vec![], to_exact: false };
    p.b.scan_all(false).unwrap();
    p.b.sync_from(&evil, None, PullOptions::default()).unwrap();
    assert!(is_link(&p.rb().join("u/x")));
    assert!(empty(outside.path()), "wrote through the symlink");
    assert_eq!(warnings(&p.b, "x/datei"), 1);
}

/// A local symlink `x` must not receive the peer's `x/datei`.
#[test]
fn writes_below_a_local_symlink_are_skipped() {
    let p = Pair::new();
    let outside = tempfile::tempdir().unwrap();
    write(&p.ra(), "u/x/datei", "payload");
    write(&p.rb(), "u/other", "b");
    std::os::unix::fs::symlink(outside.path(), p.rb().join("u/x")).unwrap();
    p.sync();
    assert!(empty(outside.path()), "wrote through the symlink");
}

/// The peer replaces folder `x` by a link: deleting `x/datei` afterwards must
/// not delete the file of the same name at the link target.
#[test]
fn deletes_below_a_new_symlink_stay_inside() {
    let p = Pair::new();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("datei"), "do not delete").unwrap();
    write(&p.ra(), "u/x/datei", "payload");
    p.sync();
    fs::remove_dir_all(p.ra().join("u/x")).unwrap();
    std::os::unix::fs::symlink(outside.path(), p.ra().join("u/x")).unwrap();
    p.sync();
    assert!(is_link(&p.rb().join("u/x")));
    assert_eq!(fs::read_to_string(outside.path().join("datei")).unwrap(), "do not delete");
}

/// Entries the peer adds to an exact-path answer are ignored.
#[test]
fn unrequested_entries_are_ignored() {
    let p = Pair::new();
    for i in 0..40 {
        write(&p.ra(), &format!("u/d/f{i:02}"), "same");
    }
    write(&p.ra(), "u/secret", "payload");
    p.sync();
    write(&p.ra(), "u/d/f07", "changed");
    p.a.scan_all(false).unwrap();
    let evil = EvilPeer { inner: &p.a, swap: None, extra: vec![entry_as(&p.a, "secret", "d/evil")], to_exact: true };
    p.b.scan_all(false).unwrap();
    p.b.sync_from(&evil, None, PullOptions::default()).unwrap();
    assert_eq!(read(&p.rb(), "u/d/f07"), "changed");
    assert!(!p.rb().join("u/d/evil").exists());
}
