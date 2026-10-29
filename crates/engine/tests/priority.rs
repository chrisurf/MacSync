//! Units the user puts first are pulled next, also in the middle of a round.

mod common;

use std::cell::RefCell;
use std::collections::BTreeSet;

use common::*;
use macsync_engine::{PullOptions, PullQueue};

/// Hands out `units` once `after` was pulled (immediately if `None`).
struct Queue {
    after: Option<&'static str>,
    units: RefCell<Vec<String>>,
    done: RefCell<Vec<String>>,
}

impl PullQueue for Queue {
    fn next(&self, done: &BTreeSet<String>) -> Option<String> {
        if self.after.is_some_and(|a| !done.contains(a)) {
            return None;
        }
        self.units.borrow().iter().find(|u| !done.contains(*u)).cloned()
    }
    fn done(&self, unit: &str) {
        self.units.borrow_mut().retain(|u| u != unit);
        self.done.borrow_mut().push(unit.to_string());
    }
}

fn setup() -> Pair {
    let p = Pair::new();
    for u in ["a", "b", "c", "d"] {
        write(&p.ra(), &format!("{u}/datei"), u);
    }
    p.a.scan_all(false).unwrap();
    p.b.scan_all(false).unwrap();
    p
}

fn order(p: &Pair, q: &Queue, only: Option<&[String]>) -> Vec<String> {
    let r = p.b.sync_from_queue(&p.a, only, PullOptions::default(), q).unwrap();
    r.into_iter().map(|r| r.unit).collect()
}

#[test]
fn queued_unit_comes_first() {
    let p = setup();
    let q = Queue { after: None, units: RefCell::new(vec!["c".into()]), done: RefCell::default() };
    assert_eq!(order(&p, &q, None), ["c", ".", "a", "b", "d"]);
    assert_eq!(*q.done.borrow(), ["c"]);
    assert_eq!(read(&p.rb(), "c/datei"), "c");
}

#[test]
fn queued_during_a_round_runs_next() {
    let p = setup();
    let q = Queue { after: Some("a"), units: RefCell::new(vec!["d".into()]), done: RefCell::default() };
    assert_eq!(order(&p, &q, None), [".", "a", "d", "b", "c"]);
}

#[test]
fn queued_unit_outside_only_and_unknown_units() {
    let p = setup();
    let q = Queue { after: None, units: RefCell::new(vec!["fehlt".into(), "d".into()]), done: RefCell::default() };
    assert_eq!(order(&p, &q, Some(&["a".to_string()])), ["d", "a"]);
    // Units the peer does not have are reported done, so they leave the queue.
    assert_eq!(*q.done.borrow(), ["fehlt", "d"]);
    assert!(q.units.borrow().is_empty());
}
