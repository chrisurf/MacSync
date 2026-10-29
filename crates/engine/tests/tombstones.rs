//! Cost of a large number of deleted entries (tombstones) in one unit, as
//! `cargo clean` leaves behind. Ignored by default:
//!
//! `cargo test --release -p macsync-engine --test tombstones -- --ignored --nocapture`

mod common;

use std::time::Instant;

use common::*;
use macsync_core::vv::Ordering;
use macsync_core::{Entry, Kind};
use macsync_store::Stored;

#[test]
#[ignore]
fn one_change_in_a_unit_full_of_tombstones() {
    let p = Pair::new();
    for i in 0..20_000 {
        write(&p.ra(), &format!("w/d{}/g{i}", i % 200), "x");
    }
    p.sync();
    for e in [&p.a, &p.b] {
        e.store()
            .atomically(|st| {
                for i in 0..400_000 {
                    let entry = Entry::tombstone(&format!("target/t{}/f{i}", i % 1000), Kind::File, Default::default());
                    st.put_entry("w", &Stored { entry, disk: String::new(), stat: Default::default(), changed_ms: 0 })?;
                }
                Ok(())
            })
            .unwrap();
        e.scan_unit("w", &Default::default()).unwrap();
    }
    let t = Instant::now();
    p.a.scan_unit("w", &Default::default()).unwrap();
    eprintln!("idle scan: {:?}", t.elapsed());

    write(&p.ra(), "w/d1/g1", "changed");
    let t = Instant::now();
    p.a.scan_unit("w", &Default::default()).unwrap();
    eprintln!("scan with one change: {:?}", t.elapsed());
    let t = Instant::now();
    let r = p.b.sync_from(&p.a, Some(&["w".to_string()]), Default::default()).unwrap();
    eprintln!("pull of one change: {:?} {:?}", t.elapsed(), r.iter().map(|r| &r.timings_ms).collect::<Vec<_>>());
    assert_eq!(read(&p.rb(), "w/d1/g1"), "changed");
}

/// A file that comes back after its deletion was synced continues its
/// version vector (the tombstone is looked up path by path) and wins on the
/// other Mac instead of turning into a conflict.
#[test]
fn deleted_and_recreated_file_follows_its_tombstone() {
    let p = Pair::new();
    write(&p.ra(), "w/f.txt", "one");
    p.sync();
    std::fs::remove_file(p.ra().join("w/f.txt")).unwrap();
    p.sync();
    assert!(!p.rb().join("w/f.txt").exists());
    let tomb = p.a.store().entry("w", "f.txt").unwrap().unwrap();
    assert!(tomb.entry.deleted);

    write(&p.ra(), "w/f.txt", "two");
    p.a.scan_unit("w", &Default::default()).unwrap();
    let back = p.a.store().entry("w", "f.txt").unwrap().unwrap();
    assert!(!back.entry.deleted);
    assert_eq!(back.entry.vv.compare(&tomb.entry.vv), Ordering::Dominates);
    p.sync();
    assert_eq!(read(&p.rb(), "w/f.txt"), "two");
    assert!(p.b.store().conflicts(false).unwrap().is_empty());
    p.assert_identical();
}

/// A build directory that exists on one Mac only, below which hundreds of
/// thousands of earlier files were deleted: the pull must not exchange all
/// those tombstones.
#[test]
#[ignore]
fn new_build_directory_above_many_tombstones() {
    let p = Pair::new();
    write(&p.ra(), "w/src/main.rs", "fn main() {}");
    p.sync();
    for e in [&p.a, &p.b] {
        e.store()
            .atomically(|st| {
                for i in 0..400_000 {
                    let entry =
                        Entry::tombstone(&format!("target/debug/t{}/f{i}", i % 1000), Kind::File, Default::default());
                    st.put_entry("w", &Stored { entry, disk: String::new(), stat: Default::default(), changed_ms: 0 })?;
                }
                Ok(())
            })
            .unwrap();
        e.scan_unit("w", &Default::default()).unwrap();
    }
    write(&p.ra(), "w/target/debug/app", "binary");
    p.a.scan_unit("w", &Default::default()).unwrap();
    let t = Instant::now();
    let r = p.b.sync_from(&p.a, Some(&["w".to_string()]), Default::default()).unwrap();
    eprintln!(
        "pull of a new build directory: {:?} {:?}",
        t.elapsed(),
        r.iter().map(|r| &r.timings_ms).collect::<Vec<_>>()
    );
    assert_eq!(read(&p.rb(), "w/target/debug/app"), "binary");
}
