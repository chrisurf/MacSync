//! "This Mac wins": make the local state of a folder or file the newer one,
//! so that the other Mac takes it over completely – its differing files are
//! replaced, files only it has are removed (both into its MacSync trash).

use std::collections::BTreeSet;

use anyhow::{bail, Result};
use macsync_core::{path as rpath, Entry};
use macsync_store::Stored;
use serde::Serialize;

use crate::capture::ScanOptions;
use crate::pull::{descend, fetch_entries, local_subtree};
use crate::{Engine, PeerApi};

#[derive(Debug, Default, Serialize)]
pub struct WinReport {
    pub unit: String,
    /// Entries that now win (newer version vector here).
    pub wins: usize,
    /// Files only the other Mac has; they are removed there.
    pub removes_there: usize,
    /// Open conflicts in the area, closed in favour of this Mac.
    pub conflicts_closed: usize,
}

impl Engine {
    /// Make this Mac's state of `prefix` in `unit` (empty: the whole unit)
    /// win against `peer`. Only the index changes here; the other Mac
    /// applies it with its next pull.
    pub fn win(&self, peer: &dyn PeerApi, unit: &str, prefix: &str) -> Result<WinReport> {
        crate::validate_unit_name(unit)?;
        let lock = self.unit_lock(unit);
        let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
        // Decide on what is on disk now.
        let s = self.scan_unit_locked(unit, &ScanOptions::default())?;
        if let Some(d) = s.deferred {
            bail!("{unit} is busy: {d}");
        }
        let tree = self.tree(unit)?;
        let remote_root = peer.roots()?.into_iter().find(|r| r.name == unit).map(|r| r.root);
        let within = |p: &str| prefix.is_empty() || p == prefix || rpath::is_within(p, prefix);
        let paths: BTreeSet<String> = match remote_root {
            Some(root) => descend(peer, unit, &tree, root, &self.cfg.excludes)?,
            None => {
                let mut all = BTreeSet::new();
                local_subtree(&tree, unit, "", &self.cfg.excludes, &mut all);
                all
            }
        }
        .into_iter()
        .filter(|p| !p.is_empty() && within(p))
        .collect();
        let remote = if remote_root.is_some() { fetch_entries(peer, unit, &paths)? } else { Default::default() };

        let me = self.cfg.device.clone();
        let now = macsync_fs::time::now_ms();
        let mut rep = WinReport { unit: unit.to_string(), ..Default::default() };
        let mut writes: Vec<Stored> = Vec::new();
        for p in &paths {
            let local = self.store().entry(unit, p)?;
            let r = remote.get(p);
            match (&local, r) {
                // Ours exists: newer than whatever the other Mac has.
                (Some(l), r) if !l.entry.deleted => {
                    if r.is_some_and(|r| !r.deleted && r.same_state(&l.entry)) {
                        continue;
                    }
                    let mut w = l.clone();
                    w.entry.vv = l.entry.vv.merge_bump(r.map_or(&l.entry.vv, |r| &r.vv), &me);
                    w.changed_ms = now;
                    writes.push(w);
                    rep.wins += 1;
                }
                // Only there: a newer deletion from here.
                (l, Some(r)) if !r.deleted => {
                    let base = l.as_ref().map(|l| l.entry.vv.clone()).unwrap_or_default();
                    writes.push(Stored {
                        entry: Entry::tombstone(p, r.kind, base.merge_bump(&r.vv, &me)),
                        disk: l.as_ref().map(|l| l.disk.clone()).unwrap_or_else(|| p.clone()),
                        stat: Default::default(),
                        changed_ms: now,
                    });
                    rep.removes_there += 1;
                }
                _ => {}
            }
        }
        let open: Vec<i64> = self
            .store()
            .conflicts(false)?
            .into_iter()
            .filter(|c| c.unit == unit && within(&c.path))
            .map(|c| c.id)
            .collect();
        for chunk in writes.chunks(crate::STORE_PAGE) {
            self.store().atomically(|st| {
                for w in chunk {
                    st.put_entry(unit, w)?;
                }
                Ok(())
            })?;
        }
        {
            let store = self.store();
            for id in &open {
                store.resolve_conflict(*id, "local")?;
            }
            let what = if prefix.is_empty() { "whole unit".to_string() } else { prefix.to_string() };
            store.log(
                unit,
                "win",
                prefix,
                &format!("this Mac's state wins ({what})"),
                &format!("{} newer here, {} removed there", rep.wins, rep.removes_there),
            )?;
            if !open.is_empty() {
                let left = store.open_conflict_count(unit)?;
                store.set_unit_status(unit, if left > 0 { "conflict" } else { "resolved" }, false)?;
            }
        }
        rep.conflicts_closed = open.len();
        self.invalidate_tree(unit)?;
        crate::telemetry::event(
            "win",
            serde_json::json!({"unit": unit, "prefix": prefix, "wins": rep.wins, "removes_there": rep.removes_there,
                               "conflicts_closed": rep.conflicts_closed}),
        );
        Ok(rep)
    }
}
