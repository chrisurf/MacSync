//! Spelling changes of unit directories on case-insensitive volumes.
//!
//! `obsidian` and `Obsidian` are one directory to APFS but two unit names to
//! the index. A rename that only changes case (or Unicode normalisation) must
//! therefore move the unit row, never create a second unit: two units over
//! the same files capture every change twice with independent version
//! vectors, which ends in endless case collisions and concurrent edits.
//!
//! The device that renamed the directory records it (`kv`); the peer sees the
//! new spelling, finds the old one locally and follows by renaming its
//! directory. Until the peer has followed, the old spelling it still
//! announces is ignored here.

use std::collections::BTreeSet;

use anyhow::Result;
use macsync_core::entry::ROOT_UNIT;
use macsync_core::path as rpath;

use crate::Engine;

/// `kv` key prefix: the unit named by the suffix got its spelling here; the
/// value is the previous spelling.
const RESPELLED: &str = "unit-respelled:";

impl Engine {
    /// Bring unit names in line with the spelling of their directories on
    /// disk. A unit whose directory is now spelled differently is renamed;
    /// if a unit with the new spelling exists already, the old row is a stale
    /// duplicate and is retired (no tombstones: the files are not gone).
    pub(crate) fn reconcile_unit_spelling(&self) -> Result<()> {
        if !self.case_insensitive {
            return Ok(());
        }
        let mut names: Vec<String> =
            self.store().units()?.into_iter().map(|u| u.name).filter(|n| n != ROOT_UNIT).collect();
        names.sort_by_key(|n| (rpath::depth(n), n.clone()));
        for name in names {
            if self.store().unit(&name)?.is_none() {
                continue; // moved along with its parent
            }
            let Some(actual) = macsync_fs::spelling_on_disk(&self.cfg.root, &name) else {
                continue; // gone: the normal scan turns it into tombstones
            };
            if actual == name {
                continue;
            }
            if self.store().unit(&actual)?.is_some() {
                self.retire_variant(&name, &actual)?;
            } else {
                self.respell_unit(&name, &actual)?;
            }
            // Either way the spelling on this disk was chosen here. If the peer
            // still announces another one, wait for it to follow instead of
            // renaming back (two devices renaming each other forever).
            self.store().set_kv(&format!("{RESPELLED}{actual}"), &name)?;
        }
        Ok(())
    }

    /// Called before pulling a unit the peer announces. Returns a reason if
    /// the pull must be skipped because the name is another spelling of a
    /// local unit; follows the peer's spelling change where that is safe.
    pub(crate) fn respell_for_pull(&self, unit: &str, remote_names: &BTreeSet<String>) -> Result<Option<String>> {
        if !self.case_insensitive || unit == ROOT_UNIT {
            return Ok(None);
        }
        if self.store().unit(unit)?.is_some() {
            // The peer uses our spelling: a rename recorded here is complete
            // once it no longer announces the old one.
            let key = format!("{RESPELLED}{unit}");
            let old = self.store().kv(&key)?;
            if let Some(old) = old {
                if !remote_names.contains(&old) {
                    self.store().delete_kv(&key)?;
                }
            }
            return Ok(None);
        }
        let ck = rpath::case_key(unit);
        let Some(other) =
            self.store().units()?.into_iter().map(|u| u.name).find(|n| n != unit && rpath::case_key(n) == ck)
        else {
            return Ok(match macsync_fs::spelling_on_disk(&self.cfg.root, unit) {
                Some(d) if d != unit => Some(format!("spelled {d:?} here")),
                _ => None,
            });
        };
        if remote_names.contains(&other) {
            return Ok(Some(format!("stale case variant of {other:?} on the peer")));
        }
        if self.store().kv(&format!("{RESPELLED}{other}"))?.is_some() {
            return Ok(Some(format!("renamed to {other:?} here; waiting for the peer to follow")));
        }
        // The peer changed the spelling: rename our directory, then the rows.
        respell_dir(&self.cfg.root, unit)?;
        self.respell_unit(&other, unit)?;
        Ok(None)
    }

    /// Rename unit `old` (and units below it) to `new` in the index.
    fn respell_unit(&self, old: &str, new: &str) -> Result<()> {
        let lock = self.unit_lock(old);
        let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
        // Nested rows that already exist under the new spelling are stale.
        let prefix = format!("{old}/");
        let names: Vec<String> = self.store().units()?.into_iter().map(|u| u.name).collect();
        for n in names {
            if let Some(rest) = n.strip_prefix(&prefix) {
                let target = format!("{new}/{rest}");
                if self.store().unit(&target)?.is_some() {
                    self.retire_variant(&n, &target)?;
                }
            }
        }
        self.store().rename_unit(old, new)?;
        let _ =
            self.store().log(new, "rename", "", "unit directory renamed (spelling only)", &format!("{old} → {new}"));
        tracing::info!(old, new, "unit respelled");
        self.trees.lock().unwrap_or_else(|e| e.into_inner()).clear();
        Ok(())
    }

    fn retire_variant(&self, stale: &str, real: &str) -> Result<()> {
        let closed = self.store().retire_unit(stale)?;
        let _ = self.store().log(
            real,
            "retire",
            "",
            &format!("stale unit {stale:?} removed (same directory, other spelling)"),
            &format!("{closed} conflicts closed"),
        );
        tracing::info!(stale, real, closed, "stale case-variant unit retired");
        self.trees.lock().unwrap_or_else(|e| e.into_inner()).remove(stale);
        Ok(())
    }
}

/// Rename the directory of root-relative `target` component by component so
/// that its spelling on disk matches exactly (case-only renames are a plain
/// `rename(2)` on APFS).
fn respell_dir(root: &std::path::Path, target: &str) -> Result<()> {
    let comps: Vec<&str> = target.split('/').collect();
    for i in 0..comps.len() {
        let want = comps[..=i].join("/");
        let Some(actual) = macsync_fs::spelling_on_disk(root, &want) else {
            return Ok(()); // not here yet: the pull creates it
        };
        if actual != want {
            std::fs::rename(root.join(&actual), root.join(&want))?;
        }
    }
    Ok(())
}
