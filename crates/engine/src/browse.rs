//! Directory listing with the synchronisation state of every item on both
//! Macs – the data behind the sync manager of the menu bar app.
//!
//! One directory at a time (lazy, also for huge trees): equal Merkle hashes
//! mean "in sync" without looking further; only differing files are fetched
//! as entries, and their version vectors tell the direction.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use reposync_core::entry::ROOT_UNIT;
use reposync_core::merkle::Child;
use reposync_core::vv::Ordering;
use reposync_core::{path as rpath, Entry, Hash, Kind};
use serde::Serialize;

use crate::{Engine, PeerApi};

/// Presence of an item on one Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Presence {
    Present,
    Missing,
    /// Deleted there; the deletion is known (tombstone).
    Deleted,
    /// The other Mac is not reachable.
    Unknown,
}

/// What synchronisation will do with an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemState {
    Synced,
    /// This Mac has the newer version; the other Mac fetches it.
    ToPeer,
    /// The other Mac has the newer version; this Mac fetches it.
    FromPeer,
    /// Deleted here; will be deleted on the other Mac.
    DeleteOnPeer,
    /// Deleted on the other Mac; will be deleted here.
    DeleteHere,
    Conflict,
    /// A directory or unit with changes inside.
    Differs,
    /// The other Mac is not reachable.
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
pub struct BrowseItem {
    pub name: String,
    /// Unit the item belongs to.
    pub unit: String,
    /// Unit-relative path; empty for a unit itself.
    pub path: String,
    pub kind: &'static str,
    /// The item is a sync unit (top-level folder or nested repository).
    pub is_unit: bool,
    pub size: Option<u64>,
    pub mtime_ms: Option<i64>,
    pub local: Presence,
    pub remote: Presence,
    pub state: ItemState,
    pub paused: bool,
    pub conflict_id: Option<i64>,
}

fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::File => "file",
        Kind::Dir => "dir",
        Kind::Symlink => "symlink",
    }
}

impl Engine {
    /// List `dir` of `unit`; `unit == None` lists the sync root (all units
    /// plus the loose files of the root). `peer == None`: other Mac offline.
    pub fn browse(&self, peer: Option<&dyn PeerApi>, unit: Option<&str>, dir: &str) -> Result<Vec<BrowseItem>> {
        let remote_roots: Option<HashMap<String, Hash>> =
            peer.and_then(|p| p.roots().ok()).map(|r| r.into_iter().map(|u| (u.name, u.root)).collect());
        let local_roots: HashMap<String, Hash> = self.local_roots()?.into_iter().map(|u| (u.name, u.root)).collect();
        let paused: BTreeSet<String> = self.store().units()?.into_iter().filter(|u| u.paused).map(|u| u.name).collect();

        let mut items = Vec::new();
        // Units directly below this directory (top level, or nested repositories).
        let prefix = match unit {
            None => String::new(),
            Some(u) => format!("{}/", rpath::join(u, dir)),
        };
        let names: BTreeSet<&String> = local_roots.keys().chain(remote_roots.iter().flat_map(|r| r.keys())).collect();
        for name in names {
            if name == ROOT_UNIT {
                continue;
            }
            let Some(rest) = name.strip_prefix(&prefix) else { continue };
            if rest.is_empty() || rest.contains('/') {
                continue;
            }
            let l = local_roots.get(name);
            let r = remote_roots.as_ref().map(|m| m.get(name));
            let (local, remote, state) = match (l, r) {
                (_, None) => (Presence::Present, Presence::Unknown, ItemState::Unknown),
                (Some(a), Some(Some(b))) if a == b => (Presence::Present, Presence::Present, ItemState::Synced),
                (Some(_), Some(Some(_))) => (Presence::Present, Presence::Present, ItemState::Differs),
                (Some(_), Some(None)) => (Presence::Present, Presence::Missing, ItemState::ToPeer),
                (None, Some(_)) => (Presence::Missing, Presence::Present, ItemState::FromPeer),
            };
            items.push(BrowseItem {
                name: rest.to_string(),
                unit: name.clone(),
                path: String::new(),
                kind: "dir",
                is_unit: true,
                size: None,
                mtime_ms: None,
                local,
                remote,
                state,
                paused: paused.contains(name.as_str()),
                conflict_id: None,
            });
        }

        // Files and folders of the unit itself (root: the loose files).
        let (u, d) = match unit {
            None => (ROOT_UNIT, ""),
            Some(u) => (u, dir),
        };
        let remote_has_unit = remote_roots.as_ref().map(|m| m.contains_key(u));
        if local_roots.contains_key(u) || remote_has_unit == Some(true) {
            let remote_peer = match remote_has_unit {
                Some(true) => peer,
                _ => None,
            };
            items.extend(self.browse_unit(remote_peer, remote_has_unit.is_some(), u, d, paused.contains(u))?);
        }
        items.sort_by_key(|i| (i.kind != "dir", i.name.to_lowercase()));
        Ok(items)
    }

    fn browse_unit(
        &self,
        peer: Option<&dyn PeerApi>,
        online: bool,
        unit: &str,
        dir: &str,
        paused: bool,
    ) -> Result<Vec<BrowseItem>> {
        let tree = self.tree(unit)?;
        let local_children: &[Child] = tree.children(dir).unwrap_or(&[]);
        let remote_children: Option<Vec<Child>> = match peer {
            Some(p) => p
                .tree(unit, &[dir.to_string()])?
                .into_iter()
                .find(|l| l.dir == dir)
                .map(|l| l.children.unwrap_or_default()),
            // Online but the other Mac does not have this unit: everything is new there.
            None if online => Some(Vec::new()),
            None => None,
        };

        let mut by_name: BTreeMap<&str, (Option<&Child>, Option<&Child>)> = BTreeMap::new();
        for c in local_children {
            by_name.entry(c.name.as_str()).or_default().0 = Some(c);
        }
        for c in remote_children.iter().flatten() {
            by_name.entry(c.name.as_str()).or_default().1 = Some(c);
        }

        // Entries are needed for sizes (local) and for the direction of
        // differing items (both sides); fetch the remote ones in one request.
        let differing: Vec<String> = by_name
            .iter()
            .filter(|(_, (l, r))| !matches!((l, r), (Some(a), Some(b)) if a.kind == b.kind && a.hash == b.hash))
            .map(|(n, _)| rpath::join(dir, n))
            .collect();
        let remote_entries: HashMap<String, Entry> = match (peer, differing.is_empty()) {
            (Some(p), false) => p.entries(unit, &differing, &[])?.into_iter().map(|e| (e.path.clone(), e)).collect(),
            _ => HashMap::new(),
        };

        let mut items = Vec::new();
        for (name, (lc, rc)) in by_name {
            let path = rpath::join(dir, name);
            let local = self.store().entry(unit, &path)?.map(|s| s.entry);
            let remote = remote_entries.get(&path);
            let kind = lc.or(rc).map(|c| c.kind).unwrap_or(Kind::File);
            let local_p = match (&local, lc) {
                (_, Some(_)) => Presence::Present,
                (Some(e), None) if e.deleted => Presence::Deleted,
                _ => Presence::Missing,
            };
            let remote_p = match (remote_children.is_some(), rc, remote) {
                (false, _, _) => Presence::Unknown,
                (true, Some(_), _) => Presence::Present,
                (true, None, Some(e)) if e.deleted => Presence::Deleted,
                (true, None, _) => Presence::Missing,
            };
            let state = if remote_children.is_none() {
                ItemState::Unknown
            } else if matches!((lc, rc), (Some(a), Some(b)) if a.kind == b.kind && a.hash == b.hash) {
                ItemState::Synced
            } else if kind == Kind::Dir && lc.is_some() && rc.is_some() {
                ItemState::Differs
            } else {
                direction(local.as_ref(), remote)
            };
            let conflict_id = self.store().open_conflict_for(unit, &path)?.map(|c| c.id);
            let shown = if lc.is_some() { local.as_ref() } else { remote.or(local.as_ref()) };
            items.push(BrowseItem {
                name: name.to_string(),
                unit: unit.to_string(),
                path,
                kind: kind_str(kind),
                is_unit: false,
                size: shown.filter(|e| e.kind == Kind::File && !e.deleted).map(|e| e.size),
                mtime_ms: shown.filter(|e| !e.deleted && e.mtime_ns > 0).map(|e| e.mtime_ns / 1_000_000),
                local: local_p,
                remote: remote_p,
                state: if conflict_id.is_some() { ItemState::Conflict } else { state },
                paused,
                conflict_id,
            });
        }
        Ok(items)
    }
}

/// Direction for an item whose state differs, from both entries
/// (tombstones included).
fn direction(local: Option<&Entry>, remote: Option<&Entry>) -> ItemState {
    let live = |e: Option<&Entry>| e.is_some_and(|e| !e.deleted);
    match (local, remote) {
        (Some(l), Some(r)) => match l.vv.compare(&r.vv) {
            Ordering::Concurrent => ItemState::Conflict,
            Ordering::Equal => ItemState::Differs,
            Ordering::Dominates if live(Some(l)) => ItemState::ToPeer,
            Ordering::Dominates => ItemState::DeleteOnPeer,
            Ordering::Dominated if live(Some(r)) => ItemState::FromPeer,
            Ordering::Dominated => ItemState::DeleteHere,
        },
        (Some(l), None) if !l.deleted => ItemState::ToPeer,
        (None, Some(r)) if !r.deleted => ItemState::FromPeer,
        _ => ItemState::Differs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reposync_core::VersionVector;

    fn entry(path: &str, vv: &[(&str, u64)], deleted: bool) -> Entry {
        let vv = VersionVector(vv.iter().map(|(d, n)| (d.to_string(), *n)).collect());
        if deleted {
            return Entry::tombstone(path, Kind::File, vv);
        }
        let mut e = Entry::tombstone(path, Kind::File, vv);
        e.deleted = false;
        e
    }

    #[test]
    fn directions() {
        let a1 = entry("f", &[("a", 1)], false);
        let a2 = entry("f", &[("a", 2)], false);
        let b = entry("f", &[("a", 1), ("b", 1)], false);
        let a2_gone = entry("f", &[("a", 2)], true);
        assert_eq!(direction(Some(&a2), Some(&a1)), ItemState::ToPeer);
        assert_eq!(direction(Some(&a1), Some(&a2)), ItemState::FromPeer);
        assert_eq!(direction(Some(&a2), Some(&b)), ItemState::Conflict);
        assert_eq!(direction(Some(&a2_gone), Some(&a1)), ItemState::DeleteOnPeer);
        assert_eq!(direction(Some(&a1), Some(&a2_gone)), ItemState::DeleteHere);
        assert_eq!(direction(Some(&a1), None), ItemState::ToPeer);
        assert_eq!(direction(None, Some(&a1)), ItemState::FromPeer);
    }
}
