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
