//! Same-named files that are not explained by a reported folder pair.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use crate::{Ctx, FileGroup, GroupKind, Member};

pub(crate) fn find(c: &Ctx, paired: &HashSet<usize>) -> Vec<FileGroup> {
    let t = c.t;
    let mut by: HashMap<&str, Vec<usize>> = HashMap::new();
    for i in t.files() {
        // Empty files (`__init__.py`, placeholders) are equal everywhere.
        if t.nodes[i].size > 0 {
            by.entry(t.nodes[i].key.as_str()).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for (_, files) in by {
        // Only copies inside folder pairs: already reported there.
        if files.len() < 2 || files.iter().all(|f| paired.contains(f)) {
            continue;
        }
        let mut clusters: Vec<Vec<usize>> = Vec::new();
        for &f in &files {
            match clusters.iter_mut().find(|cl| c.same(cl[0], f)) {
                Some(cl) => cl.push(f),
                None => clusters.push(vec![f]),
            }
        }
        // Hard links share their blocks: deleting one frees nothing.
        let reclaimable_bytes = clusters
            .iter()
            .map(|cl| {
                let inodes: HashSet<_> = cl.iter().map(|&f| (t.nodes[f].dev, t.nodes[f].ino)).collect();
                (inodes.len() as u64 - 1) * t.nodes[cl[0]].size
            })
            .sum();
        let content: HashMap<usize, usize> =
            clusters.iter().enumerate().flat_map(|(k, cl)| cl.iter().map(move |&f| (f, k + 1))).collect();
        let mut members: Vec<Member> = files
            .iter()
            .map(|&f| {
                let n = &t.nodes[f];
                Member {
                    path: n.abs.clone(),
                    display: c.display(f),
                    size: n.size,
                    mtime_ms: n.mtime_ms,
                    birth_ms: n.birth_ms,
                    content: content[&f],
                    newest: false,
                    paired: paired.contains(&f),
                    dataless: n.dataless,
                }
            })
            .collect();
        members.sort_by(|x, y| y.mtime_ms.cmp(&x.mtime_ms).then_with(|| x.display.cmp(&y.display)));
        members[0].newest = true;
        let name = t.nodes[files[0]].abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let kind = if clusters.len() == 1 { GroupKind::Duplicates } else { GroupKind::Versions };
        out.push(FileGroup { name, kind, members, reclaimable_bytes });
    }
    // Duplicates first, biggest savings first; then versions, biggest first.
    out.sort_by_key(|g| {
        let size = g.members.iter().map(|m| m.size).max().unwrap_or(0);
        (g.kind == GroupKind::Versions, Reverse(g.reclaimable_bytes), Reverse(size), g.name.clone())
    });
    out
}
