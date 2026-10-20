//! Property test: random edits on two sides, interleaved with partial syncs.
//!
//! Invariants after a final reconciliation:
//! 1. Both sides converge to identical roots and trees once all conflicts are
//!    resolved.
//! 2. No user version is ever lost: the last version a user wrote on either
//!    side is still present in a tree, the trash or the conflict store.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use common::*;
use proptest::prelude::*;
use reposync_engine::{Choice, PullOptions};

#[derive(Clone, Debug)]
enum Step {
    Write { side: bool, file: u8, content: u8 },
    Delete { side: bool, file: u8 },
    PullIntoB,
    PullIntoA,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => (any::<bool>(), 0u8..4, 0u8..6).prop_map(|(side, file, content)| Step::Write { side, file, content }),
        1 => (any::<bool>(), 0u8..4).prop_map(|(side, file)| Step::Delete { side, file }),
        2 => Just(Step::PullIntoB),
        2 => Just(Step::PullIntoA),
    ]
}

fn path(file: u8) -> String {
    format!("u/d{}/f{}.txt", file % 2, file)
}

fn collect_contents(dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_contents(&p, out);
        } else if let Ok(s) = fs::read_to_string(&p) {
            out.insert(s);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, ..ProptestConfig::default() })]

    #[test]
    fn random_edits_converge_without_loss(steps in proptest::collection::vec(step(), 1..24)) {
        let p = Pair::new();
        write(&p.ra(), "u/seed.txt", "seed");
        p.sync();
        // Last version written by the user per (side, path).
        let mut last: BTreeMap<(bool, String), String> = BTreeMap::new();
        for (i, s) in steps.iter().enumerate() {
            match s {
                Step::Write { side, file, content } => {
                    let root = if *side { p.ra() } else { p.rb() };
                    let c = format!("v{content}-{}-{i}", if *side { "A" } else { "B" });
                    write(&root, &path(*file), &c);
                    last.insert((*side, path(*file)), c);
                }
                Step::Delete { side, file } => {
                    let root = if *side { p.ra() } else { p.rb() };
                    let _ = fs::remove_file(root.join(path(*file)));
                    last.remove(&(*side, path(*file)));
                }
                Step::PullIntoB => {
                    p.a.scan_all(false).unwrap();
                    p.b.scan_all(false).unwrap();
                    p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
                }
                Step::PullIntoA => {
                    p.a.scan_all(false).unwrap();
                    p.b.scan_all(false).unwrap();
                    p.a.sync_from(&p.b, None, PullOptions::default()).unwrap();
                }
            }
        }
        p.sync();
        // Resolve every conflict: A keeps its version, B takes A's.
        for _ in 0..3 {
            let ca = p.a.store().conflicts(false).unwrap();
            for c in ca {
                p.a.resolve(c.id, Choice::Local).unwrap();
            }
            p.sync();
            let cb = p.b.store().conflicts(false).unwrap();
            for c in cb {
                p.b.resolve(c.id, Choice::Remote).unwrap();
            }
            p.sync();
        }
        prop_assert!(p.a.store().conflicts(false).unwrap().is_empty());
        prop_assert!(p.b.store().conflicts(false).unwrap().is_empty());
        prop_assert!(p.roots_equal(), "roots differ after reconciliation");
        prop_assert_eq!(snapshot(&p.ra()), snapshot(&p.rb()));

        let mut everywhere = BTreeSet::new();
        for d in [p.ra(), p.rb(), p.a.conflict_root(), p.b.conflict_root()] {
            collect_contents(&d, &mut everywhere);
        }
        for ((side, path), c) in &last {
            prop_assert!(everywhere.contains(c), "lost {} version of {}: {:?}", if *side { "A" } else { "B" }, path, c);
        }
    }
}
