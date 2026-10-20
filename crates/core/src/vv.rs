//! Version vectors.
//!
//! Every entry carries a vector `{device: counter}`. A device that changes an
//! entry increments its own counter. Comparing two vectors yields a partial
//! order: equal, one dominates the other, or concurrent (a real conflict).
//! Wall clock time is never used for decisions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionVector(pub BTreeMap<String, u64>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ordering {
    Equal,
    /// `self` is strictly newer than `other`.
    Dominates,
    /// `other` is strictly newer than `self`.
    Dominated,
    Concurrent,
}

impl VersionVector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn single(device: &str, n: u64) -> Self {
        let mut v = Self::new();
        v.0.insert(device.to_string(), n);
        v
    }

    pub fn get(&self, device: &str) -> u64 {
        self.0.get(device).copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.values().all(|&n| n == 0)
    }

    /// Increment the counter of `device` (records a local change).
    pub fn increment(&mut self, device: &str) {
        *self.0.entry(device.to_string()).or_insert(0) += 1;
    }

    /// Point-wise maximum.
    pub fn merge(&self, other: &VersionVector) -> VersionVector {
        let mut out = self.0.clone();
        for (k, &v) in &other.0 {
            let e = out.entry(k.clone()).or_insert(0);
            if v > *e {
                *e = v;
            }
        }
        out.retain(|_, v| *v > 0);
        VersionVector(out)
    }

    /// Merge and then increment `device`: the result strictly dominates both inputs.
    pub fn merge_bump(&self, other: &VersionVector, device: &str) -> VersionVector {
        let mut m = self.merge(other);
        m.increment(device);
        m
    }

    pub fn compare(&self, other: &VersionVector) -> Ordering {
        let mut greater = false;
        let mut less = false;
        for k in self.0.keys().chain(other.0.keys()) {
            let a = self.get(k);
            let b = other.get(k);
            if a > b {
                greater = true;
            } else if a < b {
                less = true;
            }
        }
        match (greater, less) {
            (false, false) => Ordering::Equal,
            (true, false) => Ordering::Dominates,
            (false, true) => Ordering::Dominated,
            (true, true) => Ordering::Concurrent,
        }
    }

    /// Compact textual form used in the journal, e.g. `a1b2:3,c4d5:1`.
    pub fn display(&self) -> String {
        self.0
            .iter()
            .map(|(k, v)| format!("{}:{}", &k[..k.len().min(4)], v))
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn vv(pairs: &[(&str, u64)]) -> VersionVector {
        VersionVector(pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect())
    }

    #[test]
    fn basic_ordering() {
        let a = vv(&[("A", 1)]);
        let b = vv(&[("A", 1), ("B", 1)]);
        assert_eq!(a.compare(&a), Ordering::Equal);
        assert_eq!(b.compare(&a), Ordering::Dominates);
        assert_eq!(a.compare(&b), Ordering::Dominated);
        let c = vv(&[("A", 2)]);
        assert_eq!(b.compare(&c), Ordering::Concurrent);
        assert_eq!(vv(&[]).compare(&vv(&[("A", 0)])), Ordering::Equal);
    }

    #[test]
    fn merge_bump_dominates_both() {
        let a = vv(&[("A", 2)]);
        let b = vv(&[("B", 5)]);
        let m = a.merge_bump(&b, "A");
        assert_eq!(m.compare(&a), Ordering::Dominates);
        assert_eq!(m.compare(&b), Ordering::Dominates);
    }

    fn arb_vv() -> impl Strategy<Value = VersionVector> {
        proptest::collection::btree_map(
            prop_oneof![Just("A".to_string()), Just("B".to_string()), Just("C".to_string())],
            0u64..4,
            0..3,
        )
        .prop_map(VersionVector)
    }

    proptest! {
        #[test]
        fn merge_is_commutative_associative_idempotent(a in arb_vv(), b in arb_vv(), c in arb_vv()) {
            prop_assert_eq!(a.merge(&b).compare(&b.merge(&a)), Ordering::Equal);
            prop_assert_eq!(a.merge(&b).merge(&c).compare(&a.merge(&b.merge(&c))), Ordering::Equal);
            prop_assert_eq!(a.merge(&a).compare(&a), Ordering::Equal);
        }

        #[test]
        fn merge_is_upper_bound(a in arb_vv(), b in arb_vv()) {
            let m = a.merge(&b);
            prop_assert!(matches!(m.compare(&a), Ordering::Equal | Ordering::Dominates));
            prop_assert!(matches!(m.compare(&b), Ordering::Equal | Ordering::Dominates));
        }

        #[test]
        fn compare_is_antisymmetric(a in arb_vv(), b in arb_vv()) {
            let expected = match a.compare(&b) {
                Ordering::Equal => Ordering::Equal,
                Ordering::Dominates => Ordering::Dominated,
                Ordering::Dominated => Ordering::Dominates,
                Ordering::Concurrent => Ordering::Concurrent,
            };
            prop_assert_eq!(b.compare(&a), expected);
        }
    }
}
