//! Resolving recorded conflicts: keep the local version, take the remote
//! version from the conflict store, or keep both.

use anyhow::{bail, Context, Result};
use reposync_core::{path as rpath, ConflictClass, Entry, Hash, Kind};
use reposync_fs::ops;
use reposync_store::Stored;

use crate::capture::ScanOptions;
use crate::plan::{class_of, rank_of, Op, OpKind, Phase, Plan, Source};
use crate::Engine;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Keep the local version; the peer will take it.
    Local,
    /// Take the remote version from the conflict store.
    Remote,
    /// Keep the local version and add the remote one as a sibling file.
    Both,
}

impl Choice {
    pub fn parse(s: &str) -> Option<Choice> {
        Some(match s {
            "local" | "mine" => Choice::Local,
            "remote" | "theirs" => Choice::Remote,
            "both" => Choice::Both,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Choice::Local => "local",
            Choice::Remote => "remote",
            Choice::Both => "both",
        }
    }
}

impl Engine {
    /// Resolve conflict `id`. Returns a human readable summary.
    pub fn resolve(&self, id: i64, choice: Choice) -> Result<String> {
        let c = self.store().conflict(id)?.with_context(|| format!("no conflict #{id}"))?;
        if c.resolved_ms.is_some() {
            bail!("conflict #{id} is already resolved ({})", c.resolution.unwrap_or_default());
        }
        let unit = c.unit.clone();
        let lock = self.unit_lock(&unit);
        let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
        // Make sure the index reflects the disk before deciding anything.
        let s = self.scan_unit_locked(&unit, &ScanOptions::default())?;
        if let Some(d) = s.deferred {
            bail!("unit {unit} is busy: {d}");
        }
        let me = self.cfg.device.clone();
        let class = ConflictClass::parse(&c.class);

        match choice {
            Choice::Local | Choice::Both => {
                if choice == Choice::Both
                    && (c.remotes.len() != 1 || c.remotes[0].kind != Kind::File || c.remotes[0].deleted)
                {
                    bail!("'both' is only possible for a single file conflict; use local or remote");
                }
                if class != Some(ConflictClass::CaseCollision) {
                    let store = self.store();
                    store.atomically(|st| {
                        for r in &c.remotes {
                            let cur = st.entry(&unit, &r.path)?;
                            let row = match cur {
                                Some(mut s) => {
                                    s.entry.vv = s.entry.vv.merge_bump(&r.vv, &me);
                                    s.changed_ms = reposync_fs::time::now_ms();
                                    s
                                }
                                None => Stored {
                                    entry: Entry::tombstone(&r.path, r.kind, r.vv.merge_bump(&r.vv, &me)),
                                    disk: r.path.clone(),
                                    stat: Default::default(),
                                    changed_ms: reposync_fs::time::now_ms(),
                                },
                            };
                            st.put_entry(&unit, &row)?;
                        }
                        Ok(())
                    })?;
                }
                if choice == Choice::Both {
                    let r = &c.remotes[0];
                    let src = std::path::Path::new(&c.stored).join(&r.path);
                    let sibling = sibling_name(&r.path, &c.remotes[0].vv.0.keys().next().cloned().unwrap_or_default());
                    let dest = reposync_fs::abs_path(&self.cfg.root, &unit, &sibling);
                    if let Some(p) = dest.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    ops::clone_or_copy(&src, &dest).with_context(|| format!("copy {}", src.display()))?;
                }
                self.invalidate_tree(&unit)?;
            }
            Choice::Remote => {
                let staging = self.staging_root().join(format!("resolve-{id}-{}", crate::new_tx_id()));
                std::fs::create_dir_all(&staging)?;
                let r = self.apply_conflict_remote(&c, &staging);
                if self.store().tx_pending()?.iter().all(|t| !staging.ends_with(&t.id)) {
                    let _ = ops::remove_any(&staging);
                }
                r?;
            }
        }
        self.store().resolve_conflict(id, choice.as_str())?;
        self.store().log(&unit, "resolve", &c.path, &format!("conflict #{id} resolved: {}", choice.as_str()), "")?;
        self.scan_unit_locked(&unit, &ScanOptions::default())?;
        let open = self.store().open_conflict_count(&unit)?;
        self.store().set_unit_status(&unit, if open > 0 { "conflict" } else { "resolved" }, false)?;
        Ok(format!("conflict #{id} ({} {}) resolved: {}", c.class, c.path, choice.as_str()))
    }

    fn apply_conflict_remote(&self, c: &reposync_store::ConflictRow, staging: &std::path::Path) -> Result<()> {
        let unit = c.unit.as_str();
        let kind = self.store().unit(unit)?.map(|u| u.kind).unwrap_or(reposync_core::UnitKind::Folder);
        let me = self.cfg.device.clone();
        let tx = staging.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut plan = Plan {
            tx,
            unit: unit.to_string(),
            unit_kind: kind,
            ops: Vec::new(),
            conflicts: Vec::new(),
            lock: None,
            actions: Vec::new(),
        };
        for r in &c.remotes {
            let local = self.store().entry(unit, &r.path)?;
            let mut e = r.clone();
            e.vv = match &local {
                Some(l) => l.entry.vv.merge_bump(&r.vv, &me),
                None => r.vv.merge_bump(&r.vv, &me),
            };
            let class = class_of(kind, &r.path);
            let disk = local.as_ref().map(|l| l.disk.clone()).unwrap_or_else(|| r.path.clone());
            let live_local = local.as_ref().filter(|l| !l.entry.deleted);
            let (op_kind, phase) = if r.deleted {
                if live_local.is_none() {
                    (OpKind::DbOnly, Phase::Db)
                } else if class.is_some_and(|c| c.is_object()) {
                    (OpKind::Delete, Phase::ObjectDeletes)
                } else {
                    (OpKind::Delete, Phase::Deletes)
                }
            } else {
                match r.kind {
                    Kind::Dir if live_local.is_some_and(|l| l.entry.kind == Kind::Dir) => (OpKind::Meta, Phase::Writes),
                    Kind::Dir => (OpKind::Mkdir, Phase::Dirs),
                    Kind::Symlink => (OpKind::Symlink, Phase::Writes),
                    Kind::File if r.path == ".git/index" => (OpKind::Write, Phase::Index),
                    Kind::File => (OpKind::Write, Phase::Writes),
                }
            };
            let staged = (op_kind == OpKind::Write).then(|| format!("{}", plan.ops.len()));
            if let Some(name) = &staged {
                let src = std::path::Path::new(&c.stored).join(if r.path.is_empty() { "_unit_root" } else { &r.path });
                let dst = staging.join(name);
                ops::clone_or_copy(&src, &dst).with_context(|| format!("conflict store copy of {}", r.path))?;
                if Some(ops::hash_file(&dst)?) != r.blob {
                    bail!("conflict store content of {} is corrupt", r.path);
                }
            }
            plan.ops.push(Op {
                path: r.path.clone(),
                disk,
                kind: op_kind,
                phase,
                rank: rank_of(class),
                entry: e,
                staged,
                source: Some(Source::File { path: c.stored.clone(), hash: r.blob.unwrap_or(Hash::ZERO) }),
                reason: format!("conflict resolved: remote version taken ({})", c.class),
            });
        }
        plan.sort();
        if kind == reposync_core::UnitKind::Git {
            let g = self.gate.check(&reposync_fs::unit_dir(&self.cfg.root, unit));
            if !g.is_open() {
                bail!("unit {unit} is busy: {}", g.describe());
            }
        }
        self.execute(&mut plan, staging)?;
        let unit_dir = reposync_fs::unit_dir(&self.cfg.root, unit);
        if kind == reposync_core::UnitKind::Git && reposync_gitaware::cmd::available() {
            let _ = reposync_gitaware::cmd::refresh_index(&unit_dir);
        }
        Ok(())
    }
}

/// `dir/name.ext` → `dir/name.conflict-<device>.ext`.
fn sibling_name(path: &str, device: &str) -> String {
    let dir = rpath::parent(path).unwrap_or("");
    let name = rpath::file_name(path);
    let dev = &device[..device.len().min(8)];
    let ts = reposync_fs::time::compact_utc(reposync_fs::time::now_ms() / 1000);
    let new = match name.rfind('.') {
        Some(i) if i > 0 => format!("{}.conflict-{dev}-{ts}{}", &name[..i], &name[i..]),
        _ => format!("{name}.conflict-{dev}-{ts}"),
    };
    rpath::join(dir, &new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_names() {
        let s = sibling_name("src/main.rs", "abcdef0123456789");
        assert!(s.starts_with("src/main.conflict-abcdef01-") && s.ends_with(".rs"), "{s}");
        let s = sibling_name("Makefile", "ab");
        assert!(s.starts_with("Makefile.conflict-ab-"), "{s}");
    }
}
