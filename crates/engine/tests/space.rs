//! The other Mac has more than fits on this disk: nothing is written, and
//! the unit says so with the sizes, until a pull gets through again.

mod common;

use common::*;
use macsync_engine::{Outcome, PullOptions};

#[test]
fn a_pull_that_does_not_fit_says_how_much_is_missing() {
    let p = Pair::new();
    write(&p.ra(), "Photos/big.jpg", &"x".repeat(4096));
    p.a.scan_all(false).unwrap();
    p.b.scan_all(false).unwrap();

    p.b.pretend_free_space(1000);
    let r = p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    let failed = r.iter().find(|r| r.unit == "Photos").expect("Photos pulled");
    assert!(
        matches!(&failed.outcome, Outcome::Failed(e) if e.contains("not enough free space")),
        "{:?}",
        failed.outcome
    );
    let (need, free) = p.b.no_space("Photos").expect("no space recorded");
    assert!(need > 4096, "{need}");
    assert_eq!(free, 1000);
    assert!(!p.rb().join("Photos/big.jpg").exists());

    // A dry run neither records nor clears it.
    p.b.sync_from(&p.a, None, PullOptions { dry_run: true }).unwrap();
    assert!(p.b.no_space("Photos").is_some());

    p.b.pretend_free_space(-1);
    p.sync();
    assert!(p.b.no_space("Photos").is_none());
    assert_eq!(read(&p.rb(), "Photos/big.jpg").len(), 4096);
}

#[test]
fn content_this_mac_already_has_needs_no_space() {
    let p = Pair::new();
    write(&p.ra(), "Photos/a.jpg", &"x".repeat(4096));
    p.sync();

    // A copy on the other Mac is cloned here, nothing comes over the network.
    write(&p.ra(), "Photos/copy.jpg", &"x".repeat(4096));
    p.a.scan_all(false).unwrap();
    p.b.pretend_free_space(1000);
    let r = p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    let rep = r.iter().find(|r| r.unit == "Photos").expect("Photos pulled");
    assert!(matches!(rep.outcome, Outcome::Applied), "{:?}", rep.outcome);
    assert!(p.b.no_space("Photos").is_none());
    assert_eq!(read(&p.rb(), "Photos/copy.jpg").len(), 4096);
}
