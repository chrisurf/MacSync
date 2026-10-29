//! Extra transfer connections that fail do not stop a transfer: the main
//! connection takes over their part.

mod common;

use anyhow::{bail, Result};
use common::*;
use macsync_core::proto::{DirListing, UnitRoot};
use macsync_core::{Entry, Hash};
use macsync_engine::{Engine, PeerApi, PullOptions};

struct WithLanes<'a> {
    inner: &'a Engine,
}

/// A connection that breaks on the first request.
struct Broken;

impl PeerApi for Broken {
    fn roots(&self) -> Result<Vec<UnitRoot>> {
        bail!("connection reset")
    }
    fn tree(&self, _: &str, _: &[String]) -> Result<Vec<DirListing>> {
        bail!("connection reset")
    }
    fn entries(&self, _: &str, _: &[String], _: &[String]) -> Result<Vec<Entry>> {
        bail!("connection reset")
    }
    fn blobs(&self, _: &[Hash]) -> Result<Vec<Option<Vec<u8>>>> {
        bail!("connection reset")
    }
    fn blob_range(&self, _: &Hash, _: u64, _: u64) -> Result<Option<Vec<u8>>> {
        bail!("connection reset")
    }
}

impl PeerApi for WithLanes<'_> {
    fn roots(&self) -> Result<Vec<UnitRoot>> {
        self.inner.roots()
    }
    fn tree(&self, unit: &str, dirs: &[String]) -> Result<Vec<DirListing>> {
        self.inner.serve_tree(unit, dirs)
    }
    fn entries(&self, unit: &str, paths: &[String], prefixes: &[String]) -> Result<Vec<Entry>> {
        self.inner.entries(unit, paths, prefixes)
    }
    fn blobs(&self, hashes: &[Hash]) -> Result<Vec<Option<Vec<u8>>>> {
        self.inner.blobs(hashes)
    }
    fn blob_range(&self, hash: &Hash, offset: u64, len: u64) -> Result<Option<Vec<u8>>> {
        self.inner.blob_range(hash, offset, len)
    }
    fn lanes(&self, n: usize) -> Vec<Box<dyn PeerApi + Send + Sync>> {
        assert!(n >= 1, "a large transfer asks for extra connections");
        (0..n).map(|_| Box::new(Broken) as Box<dyn PeerApi + Send + Sync>).collect()
    }
}

#[test]
fn broken_lanes_fall_back_to_the_main_connection() {
    let p = Pair::new();
    let large: Vec<u8> = (0..40_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::create_dir_all(p.ra().join("u/small")).unwrap();
    std::fs::write(p.ra().join("u/large.bin"), &large).unwrap();
    for i in 0..300 {
        write(&p.ra(), &format!("u/small/f{i}.bin"), &format!("{i}").repeat(1000));
    }
    p.a.scan_all(false).unwrap();
    p.b.scan_all(false).unwrap();
    let r = p.b.sync_from(&WithLanes { inner: &p.a }, None, PullOptions::default()).unwrap();
    let u = r.iter().find(|r| r.unit == "u").unwrap();
    assert!(u.converged, "{u:?}");
    assert_eq!(std::fs::read(p.rb().join("u/large.bin")).unwrap(), large);
    assert_eq!(read(&p.rb(), "u/small/f299.bin"), "299".repeat(1000));
    assert!(p.roots_equal());
}

/// A peer whose copy of one file changes during the transfer (its old
/// content is gone).
struct Changing<'a> {
    inner: &'a Engine,
    gone: Hash,
}

impl PeerApi for Changing<'_> {
    fn roots(&self) -> Result<Vec<UnitRoot>> {
        self.inner.roots()
    }
    fn tree(&self, unit: &str, dirs: &[String]) -> Result<Vec<DirListing>> {
        self.inner.serve_tree(unit, dirs)
    }
    fn entries(&self, unit: &str, paths: &[String], prefixes: &[String]) -> Result<Vec<Entry>> {
        self.inner.entries(unit, paths, prefixes)
    }
    fn blobs(&self, hashes: &[Hash]) -> Result<Vec<Option<Vec<u8>>>> {
        let mut r = self.inner.blobs(hashes)?;
        for (h, b) in hashes.iter().zip(r.iter_mut()) {
            if *h == self.gone {
                *b = None;
            }
        }
        Ok(r)
    }
    fn blob_range(&self, hash: &Hash, offset: u64, len: u64) -> Result<Option<Vec<u8>>> {
        self.inner.blob_range(hash, offset, len)
    }
}

/// A build rewriting one output file while the other Mac pulls: after a few
/// snapshot retries the rest of the unit is applied and the changing file
/// follows in the next round, instead of the whole unit starting over.
#[test]
fn a_file_changing_during_transfer_does_not_hold_back_the_rest() {
    let p = Pair::new();
    for i in 0..20 {
        write(&p.ra(), &format!("w/target/out{i}.o"), &format!("object {i}"));
    }
    write(&p.ra(), "w/target/busy.o", "being rewritten");
    p.a.scan_all(false).unwrap();
    p.b.scan_all(false).unwrap();
    let gone = macsync_core::Hash::of(b"being rewritten");
    // First as one snapshot: the unit is retried.
    for _ in 0..2 {
        let r = p.b.sync_from(&Changing { inner: &p.a, gone }, None, PullOptions::default()).unwrap();
        let w = r.iter().find(|r| r.unit == "w").unwrap();
        assert!(matches!(w.outcome, macsync_engine::Outcome::Deferred(_)), "{w:?}");
        assert!(!p.rb().join("w/target/out7.o").exists());
    }
    // Keeps changing: the rest is applied.
    let r = p.b.sync_from(&Changing { inner: &p.a, gone }, None, PullOptions::default()).unwrap();
    let w = r.iter().find(|r| r.unit == "w").unwrap();
    assert_eq!(w.outcome, macsync_engine::Outcome::Applied, "{w:?}");
    assert_eq!(w.skipped, 1);
    assert_eq!(read(&p.rb(), "w/target/out7.o"), "object 7");
    assert!(!p.rb().join("w/target/busy.o").exists());

    // Next round, the file is available again.
    p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    assert_eq!(read(&p.rb(), "w/target/busy.o"), "being rewritten");
    p.assert_identical();
}

/// Inside `.git` the repository stays together: the unit is retried.
#[test]
fn a_git_file_changing_during_transfer_defers_the_repository() {
    if !git_available() {
        return;
    }
    let p = Pair::new();
    init_repo(&p.ra().join("r"));
    p.a.scan_all(false).unwrap();
    p.b.scan_all(false).unwrap();
    let head = std::fs::read(p.ra().join("r/.git/HEAD")).unwrap();
    let r = p.b.sync_from(&Changing { inner: &p.a, gone: macsync_core::Hash::of(&head) }, None, PullOptions::default());
    let r = r.unwrap();
    let rep = r.iter().find(|r| r.unit == "r").unwrap();
    assert!(matches!(rep.outcome, macsync_engine::Outcome::Deferred(_)), "{rep:?}");
    p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    p.assert_identical();
}
