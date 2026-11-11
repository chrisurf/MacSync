//! Git state conflicts that are none: one Mac is only behind.
//!
//! Both Macs may write HEAD and refs for the same history – a `git pull` on
//! each, or a commit on one while the other still moves to the previous
//! commit. Their version vectors are then concurrent, but if one side's refs
//! are all part of the other side's history, taking the newer side is a
//! fast-forward and loses nothing. Real divergence stays a conflict.
//!
//! The index alone in conflict counts too: a commit on one Mac while the
//! other staged something (an editor or plugin staging files that a second
//! sync tool already delivered). The refs that differ decide the direction;
//! the index follows them. The behind Mac's index is kept as a backup.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use macsync_core::{Decision, Entry, GitClass, Hash};
use macsync_gitaware::refs::{self, Ahead};
use macsync_store::Stored;

use crate::{Engine, PeerApi};

/// What the fast-forward check did with the Git state conflict of a pull.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FastForward {
    /// No Git state conflict, or a real one: decisions unchanged.
    None,
    /// Resolved towards the newer side; `index_replaced` when this Mac's
    /// index was in conflict and gives way to the peer's.
    Resolved { ahead: Ahead, index_replaced: bool },
    /// The newer side's commits are still on their way: the Git state waits
    /// for the next pull instead of becoming a conflict.
    Waiting(Vec<String>),
}

fn is_ref(class: Option<GitClass>) -> bool {
    matches!(class, Some(GitClass::Ref | GitClass::Head | GitClass::PackedRefs))
}

impl Engine {
    pub(crate) fn fast_forward_git_state(
        &self,
        peer: &dyn PeerApi,
        unit: &str,
        local: &HashMap<String, Stored>,
        remote: &HashMap<String, Entry>,
        decisions: &mut Vec<(String, Option<GitClass>, Decision)>,
        me: &str,
    ) -> Result<FastForward> {
        let in_conflict = |c: &Option<GitClass>, d: &Decision| {
            c.is_some_and(|c| c.is_git_state()) && matches!(d, Decision::Conflict(_))
        };
        if !decisions.iter().any(|(_, c, d)| in_conflict(c, d)) {
            return Ok(FastForward::None);
        }
        // Every ref whose content differs between the Macs, in conflict or
        // not: with only the index in conflict, the refs the peer simply
        // moved on tell which side is ahead.
        let differs = |d: &Decision| {
            matches!(
                d,
                Decision::Conflict(_) | Decision::TakeRemote(_) | Decision::KeepLocal | Decision::KeepLocalWins(_)
            )
        };
        let refs: Vec<&String> =
            decisions.iter().filter(|(_, c, d)| is_ref(*c) && differs(d)).map(|(p, _, _)| p).collect();
        if refs.is_empty() {
            return Ok(FastForward::None);
        }

        let work = macsync_fs::unit_dir(&self.cfg.root, unit);
        let mut mine = BTreeMap::new();
        let mut theirs = BTreeMap::new();
        let mut wanted: Vec<(String, Hash)> = Vec::new();
        for path in &refs {
            let (Some(l), Some(r)) = (local.get(*path), remote.get(*path)) else {
                return Ok(FastForward::None);
            };
            let (Some(lb), Some(rb)) = (l.entry.blob, r.blob) else {
                return Ok(FastForward::None);
            };
            // The file as captured: a newer change on disk is decided later.
            let Ok(bytes) = std::fs::read(work.join(&l.disk)) else { return Ok(FastForward::None) };
            if Hash::of(&bytes) != lb {
                return Ok(FastForward::None);
            }
            let Some(v) = refs::ref_values(path, &bytes) else { return Ok(FastForward::None) };
            mine.extend(v);
            wanted.push(((*path).clone(), rb));
        }
        let hashes: Vec<Hash> = wanted.iter().map(|(_, h)| *h).collect();
        for ((path, h), bytes) in wanted.iter().zip(peer.blobs(&hashes)?) {
            let Some(bytes) = bytes.filter(|b| Hash::of(b) == *h) else { return Ok(FastForward::None) };
            let Some(v) = refs::ref_values(path, &bytes) else { return Ok(FastForward::None) };
            theirs.extend(v);
        }

        let ahead = refs::ahead(&work, &mine, &theirs);
        match ahead {
            Ahead::Diverged => Ok(FastForward::None),
            Ahead::Unknown => {
                // Only worth waiting for if this pull brings objects.
                let objects_coming = decisions
                    .iter()
                    .any(|(_, c, d)| c.is_some_and(|c| c.is_object()) && matches!(d, Decision::TakeRemote(_)));
                if !objects_coming {
                    return Ok(FastForward::None);
                }
                // The whole Git state waits, also the refs the peer moved on:
                // applied alone, they would leave the index as the only
                // difference, and that cannot tell a direction any more.
                let waits = |c: &Option<GitClass>, d: &Decision| {
                    c.is_some_and(|c| c.is_git_state())
                        && matches!(d, Decision::Conflict(_) | Decision::TakeRemote(_) | Decision::MergeReflog(_))
                };
                let held: Vec<String> =
                    decisions.iter().filter(|(_, c, d)| waits(c, d)).map(|(p, _, _)| p.clone()).collect();
                decisions.retain(|(_, c, d)| !waits(c, d));
                Ok(FastForward::Waiting(held))
            }
            Ahead::Local | Ahead::Remote => {
                let mut index_replaced = false;
                for (path, c, d) in decisions.iter_mut() {
                    if !in_conflict(c, d) {
                        continue;
                    }
                    let (Some(l), Some(r)) = (local.get(path.as_str()), remote.get(path.as_str())) else { continue };
                    *d = if ahead == Ahead::Remote {
                        index_replaced |= *c == Some(GitClass::Index) && !l.entry.deleted;
                        Decision::TakeRemote(l.entry.vv.merge(&r.vv))
                    } else if *c == Some(GitClass::Index) {
                        // Not bumped: the behind Mac must decide on its own
                        // pull, where it keeps its index as a backup.
                        Decision::KeepLocal
                    } else {
                        Decision::KeepLocalWins(l.entry.vv.merge_bump(&r.vv, me))
                    };
                }
                Ok(FastForward::Resolved { ahead, index_replaced })
            }
        }
    }
}
