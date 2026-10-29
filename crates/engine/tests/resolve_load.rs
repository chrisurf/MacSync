//! Resolving one conflict in a very large unit must not rescan the whole unit.

mod common;

use std::time::Instant;

use common::*;
use macsync_core::{Entry, Kind};
use macsync_engine::Choice;
use macsync_store::Stored;

#[test]
fn resolving_in_a_large_unit_is_quick() {
    let p = Pair::new();
    write(&p.ra(), "w/f.txt", "base");
    for i in 0..5_000 {
        write(&p.ra(), &format!("w/d{}/g{i}", i % 50), "x");
    }
    p.sync();
    write(&p.ra(), "w/f.txt", "edit on A");
    write(&p.rb(), "w/f.txt", "edit on B!");
    p.sync();
    // Many deleted entries, as left behind by `cargo clean` in a repository.
    p.a.store()
        .atomically(|st| {
            for i in 0..300_000 {
                let entry = Entry::tombstone(&format!("t{}/f{i}", i % 100), Kind::File, Default::default());
                st.put_entry("w", &Stored { entry, disk: String::new(), stat: Default::default(), changed_ms: 0 })?;
            }
            Ok(())
        })
        .unwrap();
    let c = p.a.store().conflicts(false).unwrap();
    assert_eq!(c.len(), 1);

    // One full scan of the unit as yardstick (debug and release differ a lot).
    let t = Instant::now();
    p.a.scan_unit("w", &Default::default()).unwrap();
    let scan = t.elapsed();
    // Edited again after the conflict: the resolution must capture it.
    write(&p.ra(), "w/f.txt", "edit on A, later");
    let t = Instant::now();
    p.a.resolve(c[0].id, Choice::Local).unwrap();
    let took = t.elapsed();
    eprintln!("full scan {scan:?}, resolve {took:?}");
    assert!(p.a.store().conflicts(false).unwrap().is_empty());
    assert_eq!(read(&p.ra(), "w/f.txt"), "edit on A, later");

    p.sync();
    assert_eq!(read(&p.rb(), "w/f.txt"), "edit on A, later");
    // Before: a full scan before and after the resolution (about 2.3 scans).
    assert!(took < scan, "resolve {took:?} vs. full scan {scan:?}");
}
