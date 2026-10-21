//! Line-wise union merge of reflogs.
//!
//! Reflog lines look like `<old> <new> <name> <<email>> <unix-ts> <tz>\t<msg>`.
//! Concurrent reflogs are merged by taking the union of both line sets and
//! sorting by timestamp (ties broken by the line bytes). The result is
//! independent of which side performs the merge, so both peers converge to
//! identical bytes.

use std::collections::BTreeSet;

pub fn merge(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut lines: BTreeSet<(i64, &[u8])> = BTreeSet::new();
    for l in a.split(|&c| c == b'\n').chain(b.split(|&c| c == b'\n')) {
        if l.is_empty() {
            continue;
        }
        lines.insert((timestamp(l).unwrap_or(i64::MAX), l));
    }
    let mut out = Vec::with_capacity(a.len() + b.len());
    for (_, l) in lines {
        out.extend_from_slice(l);
        out.push(b'\n');
    }
    out
}

fn timestamp(line: &[u8]) -> Option<i64> {
    let head = line.split(|&c| c == b'\t').next()?;
    let head = std::str::from_utf8(head).ok()?;
    let mut it = head.split_whitespace().rev();
    let _tz = it.next()?;
    it.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const Z: &str = "0000000000000000000000000000000000000000";

    fn line(new: char, ts: i64, msg: &str) -> String {
        let n: String = std::iter::repeat_n(new, 40).collect();
        format!("{Z} {n} Chris <c@x> {ts} +0200\t{msg}\n")
    }

    #[test]
    fn union_sorted_and_symmetric() {
        let common = line('a', 100, "commit (initial): a");
        let a = format!("{common}{}", line('b', 200, "commit: b"));
        let b = format!("{common}{}", line('c', 150, "commit: c"));
        let m1 = merge(a.as_bytes(), b.as_bytes());
        let m2 = merge(b.as_bytes(), a.as_bytes());
        assert_eq!(m1, m2);
        let s = String::from_utf8(m1).unwrap();
        let msgs: Vec<_> = s.lines().map(|l| l.split('\t').nth(1).unwrap()).collect();
        assert_eq!(msgs, vec!["commit (initial): a", "commit: c", "commit: b"]);
    }

    #[test]
    fn merging_is_idempotent() {
        let a = line('a', 1, "x");
        assert_eq!(merge(a.as_bytes(), a.as_bytes()), a.as_bytes());
    }
}
