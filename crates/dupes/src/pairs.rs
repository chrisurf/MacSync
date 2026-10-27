//! Same-named folders: pairing, verdict and copy events.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use crate::{Changed, Content, CopyEvent, Ctx, DirPair, DirSide, Item, Verdict, SAME_TIME_MS};

/// Name groups larger than this are skipped (e.g. hundreds of `src` folders):
/// too many pairs, almost all of them unrelated projects.
const MAX_GROUP: usize = 200;
/// At least this share of the smaller folder's paths must exist in the other.
const MIN_OVERLAP: f64 = 0.5;
/// Files created within this window count as one copy event.
const COPY_WINDOW_MS: i64 = 10 * 60 * 1000;

type Files = BTreeMap<String, usize>;

/// Reported pairs (largest first) and the files they account for.
pub(crate) fn find(c: &Ctx, notes: &mut Vec<String>) -> (Vec<DirPair>, HashSet<usize>) {
    let t = c.t;
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, n) in t.nodes.iter().enumerate() {
        if n.dir && !n.key.is_empty() {
            groups.entry(n.key.as_str()).or_default().push(i);
        }
    }
    let mut cache: HashMap<usize, Rc<Files>> = HashMap::new();
    let mut files = |d: usize| -> Rc<Files> {
        cache
            .entry(d)
            .or_insert_with(|| Rc::new(t.files_below(d).into_iter().map(|f| (t.key_below(d, f), f)).collect()))
            .clone()
    };

    // (a, b, common paths, forced)
    let mut cands: Vec<(usize, usize, usize, bool)> = Vec::new();
    if t.roots.len() == 2 {
        let (a, b) = (t.roots[0], t.roots[1]);
        cands.push((a, b, common(&files(a), &files(b)), true));
    }
    let mut names: Vec<_> = groups.into_iter().filter(|(_, v)| v.len() > 1).collect();
    names.sort();
    for (name, dirs) in names {
        if dirs.len() > MAX_GROUP {
            notes.push(format!("{} folders named \"{name}\": not compared", dirs.len()));
            continue;
        }
        for (k, &a) in dirs.iter().enumerate() {
            for &b in &dirs[k + 1..] {
                if t.within(a, b) || t.within(b, a) {
                    continue;
                }
                let (fa, fb) = (files(a), files(b));
                let n = common(&fa, &fb);
                let smaller = fa.len().min(fb.len());
                if n > 0 && n as f64 >= MIN_OVERLAP * smaller as f64 {
                    cands.push((a, b, n, false));
                }
            }
        }
    }
    // Outermost first: a pair covers the same-named pairs inside it.
    cands.sort_by_key(|&(a, b, n, forced)| (!forced, t.nodes[a].depth.min(t.nodes[b].depth), Reverse(n), a, b));

    let mut covered: HashSet<(usize, usize)> = HashSet::new();
    let mut paired: HashSet<usize> = HashSet::new();
    let mut out = Vec::new();
    for (a, b, n, forced) in cands {
        if covered.contains(&key(a, b)) {
            continue;
        }
        let (fa, fb) = (files(a), files(b));
        let p = evaluate(c, a, b, &fa, &fb);
        // A few same-named files with different contents in otherwise
        // unrelated folders (two projects' `src`) are no copy.
        if !forced && p.identical + p.unverified == 0 && n < 3 {
            continue;
        }
        let p = orient(c, p, a, b, &fa, &fb);
        cover(c, a, b, &mut covered);
        for (k, &f) in fa.iter() {
            if let Some(&g) = fb.get(k) {
                paired.insert(f);
                paired.insert(g);
            }
        }
        out.push(p);
    }
    out.sort_by_key(|p| Reverse(p.a.bytes.max(p.b.bytes)));
    (out, paired)
}

fn common(a: &Files, b: &Files) -> usize {
    let (s, l) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    s.keys().filter(|k| l.contains_key(*k)).count()
}

fn key(a: usize, b: usize) -> (usize, usize) {
    (a.min(b), a.max(b))
}

/// Mark the same-named sub-folders of a reported pair as handled.
fn cover(c: &Ctx, a: usize, b: usize, covered: &mut HashSet<(usize, usize)>) {
    covered.insert(key(a, b));
    let kids: HashMap<&str, usize> =
        c.t.nodes[b].children.iter().filter(|&&k| c.t.nodes[k].dir).map(|&k| (c.t.nodes[k].key.as_str(), k)).collect();
    for &x in &c.t.nodes[a].children {
        if c.t.nodes[x].dir {
            if let Some(&y) = kids.get(c.t.nodes[x].key.as_str()) {
                cover(c, x, y, covered);
            }
        }
    }
}

/// `newer`: the current copy becomes `a`. `diverged`: the more recently
/// changed copy becomes `a`.
fn orient(c: &Ctx, p: DirPair, a: usize, b: usize, fa: &Files, fb: &Files) -> DirPair {
    let has_a = !p.only_a.is_empty() || !p.newer_a.is_empty();
    let has_b = !p.only_b.is_empty() || !p.newer_b.is_empty();
    let swap = match p.verdict {
        Verdict::Identical => false,
        Verdict::Newer => has_b && !has_a,
        Verdict::Diverged => p.b.newest_ms > p.a.newest_ms,
    };
    if swap {
        evaluate(c, b, a, fb, fa)
    } else {
        p
    }
}

fn evaluate(c: &Ctx, a: usize, b: usize, fa: &Files, fb: &Files) -> DirPair {
    let t = c.t;
    let item =
        |d: usize, f: usize| Item { path: t.rel_below(d, f), size: t.nodes[f].size, mtime_ms: t.nodes[f].mtime_ms };
    let (mut identical, mut identical_bytes, mut unverified) = (0, 0, 0);
    let (mut only_a, mut only_b, mut newer_a, mut newer_b, mut unclear) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (k, &x) in fa {
        let Some(&y) = fb.get(k) else {
            only_a.push(item(a, x));
            continue;
        };
        let (nx, ny) = (&t.nodes[x], &t.nodes[y]);
        let dt = nx.mtime_ms - ny.mtime_ms;
        let content = c.content(x, y);
        if content == Content::Equal {
            identical += 1;
            identical_bytes += nx.size;
            continue;
        }
        if content == Content::Unknown && dt.abs() <= SAME_TIME_MS {
            unverified += 1;
            continue;
        }
        let ch = Changed {
            path: t.rel_below(a, x),
            size_a: nx.size,
            size_b: ny.size,
            mtime_a: nx.mtime_ms,
            mtime_b: ny.mtime_ms,
        };
        if dt > SAME_TIME_MS {
            newer_a.push(ch);
        } else if dt < -SAME_TIME_MS {
            newer_b.push(ch);
        } else {
            unclear.push(ch);
        }
    }
    for (k, &y) in fb {
        if !fa.contains_key(k) {
            only_b.push(item(b, y));
        }
    }
    let has_a = !only_a.is_empty() || !newer_a.is_empty();
    let has_b = !only_b.is_empty() || !newer_b.is_empty();
    let verdict = match (unclear.is_empty(), has_a, has_b) {
        (true, false, false) => Verdict::Identical,
        (true, true, false) | (true, false, true) => Verdict::Newer,
        _ => Verdict::Diverged,
    };
    let (mut sa, mut sb) = (side(c, a, fa), side(c, b, fb));
    // Changes after the later copy event show which copy was worked on.
    if let Some(at) = [sa.copied, sb.copied].iter().flatten().map(|e| e.until_ms).max() {
        let since = |f: &Files| f.values().filter(|&&i| t.nodes[i].mtime_ms > at + 60_000).count() as u64;
        sa.changed_since_copy = Some(since(fa));
        sb.changed_since_copy = Some(since(fb));
    }
    let reclaimable_bytes = match verdict {
        Verdict::Identical => sb.bytes,
        Verdict::Newer if !has_b => sb.bytes,
        _ => 0,
    };
    DirPair {
        verdict,
        a: sa,
        b: sb,
        identical,
        identical_bytes,
        unverified,
        only_a,
        only_b,
        newer_a,
        newer_b,
        unclear,
        reclaimable_bytes,
    }
}

fn side(c: &Ctx, d: usize, f: &Files) -> DirSide {
    let n = &c.t.nodes;
    let mut births: Vec<i64> = f.values().filter_map(|&i| n[i].birth_ms).collect();
    DirSide {
        path: n[d].abs.clone(),
        display: c.display(d),
        files: f.len() as u64,
        bytes: f.values().map(|&i| n[i].size).sum(),
        newest_ms: f.values().map(|&i| n[i].mtime_ms).max().unwrap_or(0),
        copied: copy_event(&mut births, f.len()),
        changed_since_copy: None,
    }
}

/// The densest `COPY_WINDOW_MS` window of creation times, if it holds at
/// least half of the files (and at least three).
pub(crate) fn copy_event(births: &mut [i64], total: usize) -> Option<CopyEvent> {
    births.sort_unstable();
    let (mut best, mut at) = (0, 0);
    let mut lo = 0;
    for hi in 0..births.len() {
        while births[hi] - births[lo] > COPY_WINDOW_MS {
            lo += 1;
        }
        if hi + 1 - lo > best {
            best = hi + 1 - lo;
            at = lo;
        }
    }
    (best >= 3 && best * 2 >= total).then(|| CopyEvent {
        at_ms: births[at],
        until_ms: births[at + best - 1],
        files: best as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_event_needs_half_of_the_files() {
        let min = 60_000;
        let mut b = vec![0, 5 * min, 100 * min, 101 * min, 102 * min, 103 * min];
        let e = copy_event(&mut b, 6).unwrap();
        assert_eq!((e.at_ms, e.until_ms, e.files), (100 * min, 103 * min, 4));
        let mut b = vec![0, 100 * min, 200 * min, 300 * min];
        assert!(copy_event(&mut b, 4).is_none());
        let mut b = vec![0, min, 2 * min];
        assert!(copy_event(&mut b, 10).is_none());
    }
}
