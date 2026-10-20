//! Decision logic: what to do with a remote entry given the local one.
//!
//! The receiving side always decides (pull model). Decisions must be
//! *symmetric*: when both sides run `decide` against each other's entry they
//! must reach the same final state without further rounds, unless the result
//! is a conflict that a human resolves.

use crate::entry::{Entry, Kind};
use crate::gitpath::GitClass;
use crate::vv::{Ordering, VersionVector};

/// Per-unit direction preference ("this Mac wins").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Prefer {
    #[default]
    None,
    Local,
    Remote,
}

impl Prefer {
    pub fn as_str(self) -> &'static str {
        match self {
            Prefer::None => "none",
            Prefer::Local => "local",
            Prefer::Remote => "remote",
        }
    }

    pub fn parse(s: &str) -> Option<Prefer> {
        match s {
            "none" | "" => Some(Prefer::None),
            "local" => Some(Prefer::Local),
            "remote" => Some(Prefer::Remote),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Both sides are equal; concurrent edits become conflicts unless a
    /// preference is set.
    TwoWay(Prefer),
    /// The remote side is authoritative (mirror receiver).
    MirrorReceive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConflictClass {
    /// Both sides changed the entry concurrently to different states.
    ConcurrentModify,
    /// Remote and local disagree about the kind (file vs directory …).
    KindChange,
    /// A concurrent change of HEAD/refs/index: the whole Git state of the unit
    /// is held back as a single conflict.
    GitState,
    /// The remote path differs from a local path only in case/normalisation
    /// on a case-insensitive volume.
    CaseCollision,
}

impl ConflictClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictClass::ConcurrentModify => "concurrent-modify",
            ConflictClass::KindChange => "kind-change",
            ConflictClass::GitState => "git-state",
            ConflictClass::CaseCollision => "case-collision",
        }
    }

    pub fn parse(s: &str) -> Option<ConflictClass> {
        Some(match s {
            "concurrent-modify" => ConflictClass::ConcurrentModify,
            "kind-change" => ConflictClass::KindChange,
            "git-state" => ConflictClass::GitState,
            "case-collision" => ConflictClass::CaseCollision,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Already in sync.
    Nothing,
    /// States are identical; only store the given version vector.
    AdoptVv(VersionVector),
    /// Apply the remote state and store the given version vector.
    TakeRemote(VersionVector),
    /// The local entry is newer; the peer will pull it.
    KeepLocal,
    /// Keep the local state and store the given version vector which strictly
    /// dominates the remote one, so that the peer takes the local state.
    KeepLocalWins(VersionVector),
    /// Concurrent reflog changes: write the line-wise union with this vector.
    /// The union is deterministic, so both sides converge to the same bytes.
    MergeReflog(VersionVector),
    /// Do not touch anything locally; record a conflict.
    Conflict(ConflictClass),
}

/// Decide what to do with `remote` given the `local` entry for the same path.
///
/// `class` is the Git class of the path for Git units, `None` for folder units.
/// `me` is the local device id.
pub fn decide(local: Option<&Entry>, remote: &Entry, class: Option<GitClass>, policy: Policy, me: &str) -> Decision {
    let Some(local) = local else {
        return Decision::TakeRemote(remote.vv.clone());
    };
    let same = local.same_state(remote);
    let merged = local.vv.merge(&remote.vv);
    let vv_equal = local.vv.compare(&remote.vv) == Ordering::Equal;

    if same {
        return if vv_equal { Decision::Nothing } else { Decision::AdoptVv(merged) };
    }

    if policy == Policy::MirrorReceive {
        return Decision::TakeRemote(merged);
    }

    match local.vv.compare(&remote.vv) {
        Ordering::Dominated => Decision::TakeRemote(remote.vv.clone()),
        Ordering::Dominates => Decision::KeepLocal,
        // Equal vectors with different states only happen after a database
        // rebuild; treat them like a concurrent change.
        Ordering::Equal | Ordering::Concurrent => concurrent(local, remote, class, policy, merged, me),
    }
}

fn concurrent(
    local: &Entry,
    remote: &Entry,
    class: Option<GitClass>,
    policy: Policy,
    merged: VersionVector,
    me: &str,
) -> Decision {
    // Modify vs. delete: the modification always wins (avoid data loss).
    if remote.deleted && !local.deleted {
        return Decision::AdoptVv(merged);
    }
    if local.deleted && !remote.deleted {
        return Decision::TakeRemote(merged);
    }

    // Classes that can be merged or tie-broken deterministically.
    let tie_break = match class {
        Some(c) if c.is_object() || c == GitClass::Ephemeral => true,
        Some(GitClass::Reflog) if local.kind == Kind::File && remote.kind == Kind::File => {
            return Decision::MergeReflog(merged);
        }
        _ => local.kind == Kind::Dir && remote.kind == Kind::Dir,
    };
    if tie_break {
        return if remote.state_hash() > local.state_hash() {
            Decision::TakeRemote(merged)
        } else {
            Decision::AdoptVv(merged)
        };
    }

    let class_of_conflict = if local.kind != remote.kind {
        ConflictClass::KindChange
    } else if class.is_some_and(|c| c.is_git_state()) {
        ConflictClass::GitState
    } else {
        ConflictClass::ConcurrentModify
    };

    match policy {
        Policy::TwoWay(Prefer::Local) => Decision::KeepLocalWins(local.vv.merge_bump(&remote.vv, me)),
        Policy::TwoWay(Prefer::Remote) => Decision::TakeRemote(merged),
        _ => Decision::Conflict(class_of_conflict),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Hash;
    use proptest::prelude::*;

    fn file(content: &str, vv: &[(&str, u64)]) -> Entry {
        let h = Hash::of(content.as_bytes());
        Entry {
            path: "f".into(),
            kind: Kind::File,
            mode: 0o644,
            size: content.len() as u64,
            mtime_ns: 0,
            blob: Some(h),
            cmp: Some(h),
            target: None,
            xattrs: vec![],
            flags: 0,
            link: None,
            deleted: false,
            vv: VersionVector(vv.iter().map(|(k, v)| (k.to_string(), *v)).collect()),
        }
    }

    fn deleted(vv: &[(&str, u64)]) -> Entry {
        let mut e = file("", vv);
        e.deleted = true;
        e
    }

    const TW: Policy = Policy::TwoWay(Prefer::None);

    #[test]
    fn new_remote_is_taken() {
        let r = file("x", &[("B", 1)]);
        assert_eq!(decide(None, &r, None, TW, "A"), Decision::TakeRemote(r.vv.clone()));
    }

    #[test]
    fn newer_remote_is_taken_older_is_kept() {
        let l = file("x", &[("A", 1)]);
        let r = file("y", &[("A", 1), ("B", 1)]);
        assert!(matches!(decide(Some(&l), &r, None, TW, "A"), Decision::TakeRemote(_)));
        assert_eq!(decide(Some(&r), &l, None, TW, "B"), Decision::KeepLocal);
    }

    #[test]
    fn identical_concurrent_change_merges_vectors() {
        let l = file("same", &[("A", 2)]);
        let r = file("same", &[("B", 1)]);
        let d = decide(Some(&l), &r, None, TW, "A");
        assert_eq!(d, Decision::AdoptVv(l.vv.merge(&r.vv)));
    }

    #[test]
    fn concurrent_edit_is_conflict() {
        let l = file("mine", &[("A", 2)]);
        let r = file("theirs", &[("A", 1), ("B", 1)]);
        assert_eq!(decide(Some(&l), &r, None, TW, "A"), Decision::Conflict(ConflictClass::ConcurrentModify));
        assert_eq!(decide(Some(&l), &r, Some(GitClass::Ref), TW, "A"), Decision::Conflict(ConflictClass::GitState));
    }

    #[test]
    fn modification_beats_deletion_on_both_sides() {
        let modified = file("new", &[("A", 2)]);
        let gone = deleted(&[("A", 1), ("B", 1)]);
        let a = decide(Some(&modified), &gone, None, TW, "A");
        let b = decide(Some(&gone), &modified, None, TW, "B");
        assert!(matches!(a, Decision::AdoptVv(_)));
        assert!(matches!(b, Decision::TakeRemote(_)));
    }

    #[test]
    fn preference_resolves_conflicts() {
        let l = file("mine", &[("A", 2)]);
        let r = file("theirs", &[("B", 1)]);
        match decide(Some(&l), &r, None, Policy::TwoWay(Prefer::Local), "A") {
            Decision::KeepLocalWins(vv) => assert_eq!(vv.compare(&r.vv), Ordering::Dominates),
            d => panic!("{d:?}"),
        }
        assert!(matches!(decide(Some(&l), &r, None, Policy::TwoWay(Prefer::Remote), "A"), Decision::TakeRemote(_)));
        assert!(matches!(decide(Some(&l), &r, None, Policy::MirrorReceive, "A"), Decision::TakeRemote(_)));
    }

    #[test]
    fn reflog_concurrent_merges() {
        let l = file("a", &[("A", 1)]);
        let r = file("b", &[("B", 1)]);
        assert!(matches!(decide(Some(&l), &r, Some(GitClass::Reflog), TW, "A"), Decision::MergeReflog(_)));
    }

    /// Result of applying a decision on one side: `None` means conflict.
    fn outcome(local: &Entry, remote: &Entry, d: Decision) -> Option<(crate::hash::Hash, VersionVector)> {
        match d {
            Decision::Nothing | Decision::KeepLocal => Some((local.state_hash(), local.vv.clone())),
            Decision::AdoptVv(vv) | Decision::KeepLocalWins(vv) => Some((local.state_hash(), vv)),
            Decision::TakeRemote(vv) => Some((remote.state_hash(), vv)),
            Decision::MergeReflog(_) | Decision::Conflict(_) => None,
        }
    }

    fn arb_entry() -> impl Strategy<Value = Entry> {
        let vv =
            proptest::collection::btree_map(prop_oneof![Just("A".to_string()), Just("B".to_string())], 0u64..3, 0..3);
        (prop_oneof![Just("x"), Just("y")], any::<bool>(), vv).prop_map(|(c, del, vv)| {
            let mut e = file(c, &[]);
            e.vv = VersionVector(vv);
            e.deleted = del;
            e
        })
    }

    fn arb_class() -> impl Strategy<Value = Option<GitClass>> {
        prop_oneof![
            Just(None),
            Just(Some(GitClass::Worktree)),
            Just(Some(GitClass::LooseObject)),
            Just(Some(GitClass::Ephemeral)),
            Just(Some(GitClass::Ref)),
        ]
    }

    proptest! {
        /// When neither side reports a conflict, both sides end in the same
        /// state with comparable vectors after one exchange in each direction.
        #[test]
        fn decisions_are_symmetric(a in arb_entry(), b in arb_entry(), class in arb_class()) {
            let da = decide(Some(&a), &b, class, TW, "A");
            let db = decide(Some(&b), &a, class, TW, "B");
            let (Some(oa), Some(ob)) = (outcome(&a, &b, da.clone()), outcome(&b, &a, db.clone())) else {
                // A conflict must be detected on both sides.
                prop_assert_eq!(matches!(da, Decision::Conflict(_)), matches!(db, Decision::Conflict(_)));
                return Ok(());
            };
            // After both sides applied their decision, one more round must be a no-op
            // or a one-directional transfer of the newer state.
            if oa.0 != ob.0 {
                let order = oa.1.compare(&ob.1);
                prop_assert!(matches!(order, Ordering::Dominates | Ordering::Dominated), "{:?} {:?} -> {:?} {:?}", a, b, da, db);
            }
        }

        /// No decision ever silently drops a live local state that is concurrent
        /// with the remote one, unless a preference or tie-break class allows it.
        #[test]
        fn no_silent_loss(a in arb_entry(), b in arb_entry()) {
            let d = decide(Some(&a), &b, None, TW, "A");
            if let Decision::TakeRemote(_) = d {
                let ord = a.vv.compare(&b.vv);
                prop_assert!(
                    ord == Ordering::Dominated || a.deleted || a.same_state(&b),
                    "lost {:?} for {:?}", a, b
                );
            }
        }
    }
}
