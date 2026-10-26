//! Reliability tests: crashes at every point of an apply, network failures
//! during transfer, and a source that changes while it is being read.

mod common;

use std::cell::Cell;
use std::fs;

use anyhow::Result;
use common::*;
use macsync_core::proto::{DirListing, UnitRoot};
use macsync_core::{Entry, Hash};
use macsync_engine::{Engine, Outcome, PeerApi, PullOptions};

/// Crash after k file operations for every k; after "restart" the repository
/// must be complete, valid and identical to the source.
#[test]
fn crash_at_every_operation_is_recovered() {
    if !git_available() {
        return;
    }
    let mut k = 0;
    loop {
        let p = Pair::new();
        let a = p.ra().join("app");
        init_repo(&a);
        p.sync();
        let b = p.rb().join("app");
        let head_before = git(&b, &["rev-parse", "HEAD"]);
        fs::write(a.join("src/main.rs"), "fn main() { println!(\"v2\"); }\n").unwrap();
        fs::write(a.join("new.txt"), "new").unwrap();
        fs::remove_file(a.join("README.md")).unwrap();
        git(&a, &["add", "-A"]);
        git(&a, &["commit", "-qm", "second"]);
        p.a.scan_all(false).unwrap();
        p.b.scan_all(false).unwrap();

        p.b.inject_crash_after(k);
        let r = p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
        let app = r.iter().find(|r| r.unit == "app").unwrap();
        let crashed = matches!(&app.outcome, Outcome::Failed(m) if m.contains("injected crash"));
        p.b.inject_crash_after(-1);

        if crashed {
            // While interrupted, Git is blocked by our lock and HEAD is either
            // old or new – never a ref to a missing object.
            let head_now = git(&b, &["rev-parse", "HEAD"]);
            assert!(head_now == head_before || head_now == git(&a, &["rev-parse", "HEAD"]), "k={k}");
            assert_fsck(&b);
            // Restart.
            let b2 = Engine::open(p.b.config().clone()).unwrap();
            assert!(b2.store().tx_pending().unwrap().is_empty(), "k={k}");
        }
        assert!(!b.join(".git/index.lock").exists(), "k={k}");
        assert_fsck(&b);
        assert_eq!(git(&a, &["rev-parse", "HEAD"]), git(&b, &["rev-parse", "HEAD"]), "k={k}");
        assert_eq!(git(&b, &["status", "--porcelain"]), "", "k={k}");
        assert_eq!(read(&b, "src/main.rs"), read(&a, "src/main.rs"), "k={k}");
        assert!(!b.join("README.md").exists(), "k={k}");
        if !crashed {
            assert!(k > 3, "the transaction should have more than 3 operations");
            eprintln!("crash points tested: {k}");
            break;
        }
        k += 1;
    }
}

/// A peer whose connection breaks after `ok_calls` blob requests.
struct FlakyPeer<'a> {
    inner: &'a Engine,
    ok_calls: Cell<i32>,
}

impl PeerApi for FlakyPeer<'_> {
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
        self.tick()?;
        self.inner.blobs(hashes)
    }
    fn blob_range(&self, hash: &Hash, offset: u64, len: u64) -> Result<Option<Vec<u8>>> {
        self.tick()?;
        self.inner.blob_range(hash, offset, len)
    }
}

impl FlakyPeer<'_> {
    fn tick(&self) -> Result<()> {
        let n = self.ok_calls.get();
        if n <= 0 {
            anyhow::bail!("connection reset by peer");
        }
        self.ok_calls.set(n - 1);
        Ok(())
    }
}

#[test]
fn network_failure_mid_transfer_leaves_old_state() {
    let p = Pair::new();
    write(&p.ra(), "d/keep.txt", "old");
    p.sync();
    // Many small files (several bundles) plus a large file (ranges).
    for i in 0..6000 {
        write(&p.ra(), &format!("d/many/f{i}.txt"), &format!("{i}"));
    }
    let big: Vec<u8> = (0..9_000_000u32).map(|i| (i % 249) as u8).collect();
    fs::write(p.ra().join("d/big.bin"), &big).unwrap();
    write(&p.ra(), "d/keep.txt", "new");
    p.a.scan_all(false).unwrap();
    p.b.scan_all(false).unwrap();
    let before = snapshot(&p.rb());

    for ok in [0, 1, 2] {
        let flaky = FlakyPeer { inner: &p.a, ok_calls: Cell::new(ok) };
        let r = p.b.sync_from(&flaky, None, PullOptions::default()).unwrap();
        let d = r.iter().find(|r| r.unit == "d").unwrap();
        assert!(matches!(d.outcome, Outcome::Failed(_)), "{:?}", d.outcome);
        assert_eq!(snapshot(&p.rb()), before, "partial state visible after failure at call {ok}");
        assert!(p.b.store().tx_pending().unwrap().is_empty());
        assert!(fs::read_dir(p.rb().join(".macsync/staging")).unwrap().next().is_none(), "staging cleaned");
    }
    // The connection is back: everything arrives.
    p.sync();
    p.assert_identical();
    assert_eq!(read(&p.rb(), "d/keep.txt"), "new");
}

/// A peer that modifies a source file between listing and transfer.
struct MutatingPeer<'a> {
    inner: &'a Engine,
    file: std::path::PathBuf,
    done: Cell<bool>,
}

impl PeerApi for MutatingPeer<'_> {
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
        if !self.done.replace(true) {
            fs::write(&self.file, "changed while transferring").unwrap();
        }
        self.inner.blobs(hashes)
    }
    fn blob_range(&self, hash: &Hash, offset: u64, len: u64) -> Result<Option<Vec<u8>>> {
        self.inner.blob_range(hash, offset, len)
    }
}

#[test]
fn source_change_during_transfer_aborts_the_transaction() {
    let p = Pair::new();
    write(&p.ra(), "s/a.txt", "a1");
    write(&p.ra(), "s/b.txt", "b1");
    p.sync();
    write(&p.ra(), "s/a.txt", "a2");
    write(&p.ra(), "s/b.txt", "b2");
    p.a.scan_all(false).unwrap();
    p.b.scan_all(false).unwrap();
    let peer = MutatingPeer { inner: &p.a, file: p.ra().join("s/b.txt"), done: Cell::new(false) };
    let r = p.b.sync_from(&peer, None, PullOptions::default()).unwrap();
    let s = r.iter().find(|r| r.unit == "s").unwrap();
    assert!(matches!(s.outcome, Outcome::Deferred(_)), "{:?}", s.outcome);
    // Neither file was applied: no mixture of old a / new b.
    assert_eq!(read(&p.rb(), "s/a.txt"), "a1");
    assert_eq!(read(&p.rb(), "s/b.txt"), "b1");
    p.sync();
    assert_eq!(read(&p.rb(), "s/a.txt"), "a2");
    assert_eq!(read(&p.rb(), "s/b.txt"), "changed while transferring");
    p.assert_identical();
}

#[test]
fn database_loss_never_overwrites_local_data() {
    let p = Pair::new();
    write(&p.ra(), "x/f.txt", "base");
    p.sync();
    // B loses its database (e.g. corruption) and edits meanwhile.
    write(&p.rb(), "x/f.txt", "B edit after db loss");
    let cfg = p.b.config().clone();
    let Pair { a, b, dir } = p;
    drop(b);
    for f in ["macsync.db", "macsync.db-wal", "macsync.db-shm"] {
        let _ = fs::remove_file(cfg.state_dir.join(f));
    }
    let b = Engine::open(cfg).unwrap();
    let p = Pair { a, b, dir };
    write(&p.ra(), "x/f.txt", "A edit");
    p.sync();
    // Rebuilt vectors are concurrent → conflict, never a silent overwrite.
    assert_eq!(read(&p.rb(), "x/f.txt"), "B edit after db loss");
    assert_eq!(read(&p.ra(), "x/f.txt"), "A edit");
    assert!(!p.b.store().conflicts(false).unwrap().is_empty());
}
