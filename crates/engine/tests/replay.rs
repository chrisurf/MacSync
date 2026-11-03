//! Another tool on the same folder (a second sync program between the same
//! devices, a restore) writes an old version of a file back while the other
//! Mac has already moved on. That is no conflict: the newer version wins on
//! both Macs. A version that is really new still makes a conflict.

mod common;

use std::time::{Duration, SystemTime};

use common::*;

fn set_mtime(path: &std::path::Path, t: SystemTime) {
    std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
}

/// v1 and v2 synced, then A writes v3 while on B the other tool writes v1
/// back with its old time.
fn replayed(p: &Pair, replay_time: SystemTime) {
    let t0 = SystemTime::now() - Duration::from_secs(3600);
    write(&p.ra(), "notes/k.md", "v1");
    set_mtime(&p.ra().join("notes/k.md"), t0);
    p.sync();
    write(&p.ra(), "notes/k.md", "v2 with more");
    set_mtime(&p.ra().join("notes/k.md"), t0 + Duration::from_secs(60));
    p.sync();
    assert_eq!(read(&p.rb(), "notes/k.md"), "v2 with more");

    write(&p.ra(), "notes/k.md", "v3 with even more");
    set_mtime(&p.ra().join("notes/k.md"), t0 + Duration::from_secs(120));
    write(&p.rb(), "notes/k.md", "v1");
    set_mtime(&p.rb().join("notes/k.md"), replay_time);
}

#[test]
fn old_version_written_back_loses_without_a_conflict() {
    let p = Pair::new();
    let t0 = SystemTime::now() - Duration::from_secs(3600);
    replayed(&p, t0);
    p.sync();
    for (e, root) in [(&p.a, p.ra()), (&p.b, p.rb())] {
        assert!(e.store().conflicts(false).unwrap().is_empty(), "{:?}", e.store().conflicts(false).unwrap());
        assert_eq!(read(&root, "notes/k.md"), "v3 with even more");
    }
    p.assert_identical();
}

/// The same old content, but written now (`git reset`, a restore that stamps
/// the time of restoring): a real change, so both versions are kept.
#[test]
fn old_content_written_now_is_a_real_change() {
    let p = Pair::new();
    replayed(&p, SystemTime::now());
    p.sync();
    let open = p.a.store().conflicts(false).unwrap().len() + p.b.store().conflicts(false).unwrap().len();
    assert!(open > 0, "a deliberate change was overwritten silently");
}

#[test]
fn new_content_on_both_macs_stays_a_conflict() {
    let p = Pair::new();
    write(&p.ra(), "notes/k.md", "v1");
    p.sync();
    write(&p.ra(), "notes/k.md", "edited on A");
    write(&p.rb(), "notes/k.md", "edited on B");
    p.sync();
    assert!(!p.a.store().conflicts(false).unwrap().is_empty() || !p.b.store().conflicts(false).unwrap().is_empty());
}

/// The lineage travels with the entry: the receiving Mac knows the earlier
/// contents without ever having had them.
#[test]
fn lineage_travels_with_the_file() {
    let p = Pair::new();
    write(&p.ra(), "notes/k.md", "one");
    p.a.scan_all(false).unwrap();
    write(&p.ra(), "notes/k.md", "two");
    p.a.scan_all(false).unwrap();
    write(&p.ra(), "notes/k.md", "three");
    p.sync();
    let e = p.b.store().entry("notes", "k.md").unwrap().unwrap().entry;
    assert_eq!(e.prev, vec![macsync_core::Hash::of(b"two"), macsync_core::Hash::of(b"one")]);
}
