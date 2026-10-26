//! Executing a plan: Git-ordered, staged, atomic per file, with the index
//! written last through our own `index.lock`, and crash recovery from the
//! transaction journal.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use macsync_core::{Kind, UnitKind};
use macsync_fs::{meta, ops, scan};
use macsync_store::{StatCache, Stored};

use crate::plan::{Op, OpKind, Phase, Plan};
use crate::pull::RetryLater;
use crate::Engine;

impl Engine {
    /// Apply a fully staged plan. The plan is journaled before the first file
    /// operation; if this function is interrupted, [`Engine::recover`] finishes
    /// the transaction.
    pub(crate) fn execute(&self, plan: &mut Plan, staging: &Path) -> Result<()> {
        let unit_dir = macsync_fs::unit_dir(&self.cfg.root, &plan.unit);
        if plan.has_disk_ops() {
            self.take_index_lock(plan, &unit_dir)?;
        }
        self.store().tx_put(&plan.tx, &plan.unit, "applying", &macsync_core::codec::encode(&*plan)?)?;
        let result = self.run_ops(plan, staging, &unit_dir);
        result?;
        self.commit(plan, "applied")?;
        Ok(())
    }

    fn take_index_lock(&self, plan: &mut Plan, unit_dir: &Path) -> Result<()> {
        if plan.unit_kind != UnitKind::Git || plan.lock.is_some() {
            return Ok(());
        }
        let Some(git_dir) = macsync_gitaware::resolve_git_dir(unit_dir) else {
            return Ok(());
        };
        let lock = git_dir.join("index.lock");
        match fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(_) => {
                plan.lock = Some(lock.to_string_lossy().into_owned());
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                Err(RetryLater("index.lock exists (git is running)".into()).into())
            }
            Err(e) => Err(e).context("create index.lock"),
        }
    }

    fn run_ops(&self, plan: &mut Plan, staging: &Path, unit_dir: &Path) -> Result<()> {
        let trash = self.trash_root().join(&plan.tx);
        for i in 0..plan.ops.len() {
            if plan.ops[i].phase > Phase::Dirs && plan.lock.is_none() && plan.unit_kind == UnitKind::Git {
                // A new repository: `.git` exists only after the directory phase.
                if macsync_gitaware::resolve_git_dir(unit_dir).is_some() {
                    self.take_index_lock(plan, unit_dir)?;
                    self.store().tx_put(&plan.tx, &plan.unit, "applying", &macsync_core::codec::encode(&*plan)?)?;
                }
            }
            if plan.ops[i].kind != OpKind::DbOnly {
                self.tick_fault()?;
            }
            let op = &plan.ops[i];
            self.apply_op(&plan.unit, op, i, staging, &trash, plan.lock.as_deref())
                .with_context(|| format!("apply {:?} {}", op.kind, op.path))?;
        }
        // No index was written: release our lock.
        if let Some(l) = &plan.lock {
            let p = Path::new(l);
            if fs::metadata(p).is_ok_and(|m| m.len() == 0) {
                let _ = fs::remove_file(p);
            }
        }
        Ok(())
    }

    fn apply_op(&self, unit: &str, op: &Op, i: usize, staging: &Path, trash: &Path, lock: Option<&str>) -> Result<()> {
        let target = macsync_fs::abs_path(&self.cfg.root, unit, &op.disk);
        let trash_rel = macsync_fs::root_rel(unit, &op.disk);
        let e = &op.entry;
        match op.kind {
            OpKind::DbOnly => {}
            OpKind::Mkdir => {
                if fs::symlink_metadata(&target).is_ok_and(|m| !m.is_dir()) {
                    ops::move_to_trash(&target, trash, &trash_rel)?;
                }
                fs::create_dir_all(&target)?;
                self.set_meta(&target, e)?;
            }
            OpKind::Meta => {
                meta::unlock_for_write(&target);
                self.set_meta(&target, e)?;
                meta::set_mtime(&target, e.kind, e.mtime_ns).ok();
                meta::set_flags(&target, e.flags).ok();
            }
            OpKind::Write => {
                let staged = staging.join(op.staged.as_deref().unwrap_or("missing"));
                if fs::symlink_metadata(&staged).is_err() {
                    return Ok(()); // already applied (recovery)
                }
                if let Some(leader) = &e.link {
                    // Re-create the hard link group when the leader has the same content.
                    let leader_abs = macsync_fs::abs_path(&self.cfg.root, unit, leader);
                    if ops::hash_file(&leader_abs).ok() == e.blob {
                        let tmp = staging.join(format!("link-{i}"));
                        let _ = fs::remove_file(&tmp);
                        if fs::hard_link(&leader_abs, &tmp).is_ok() {
                            fs::rename(&tmp, &staged)?;
                        }
                    }
                }
                // The staged file is fresh: only set attributes it should have.
                meta::set_mode(&staged, Kind::File, e.mode)?;
                if !e.xattrs.is_empty() {
                    meta::write_xattrs(&staged, &e.xattrs, self.cfg.strip_quarantine)?;
                }
                meta::set_mtime(&staged, Kind::File, e.mtime_ns)?;
                if let Some(p) = target.parent() {
                    if fs::symlink_metadata(p).is_err() {
                        fs::create_dir_all(p)?;
                    }
                }
                self.backup(&target, trash, &trash_rel)?;
                if op.phase == Phase::Index {
                    match lock {
                        Some(l) => {
                            // Exactly like Git: content into index.lock, then rename to index.
                            fs::rename(&staged, l)?;
                            fs::rename(l, &target)?;
                        }
                        None => fs::rename(&staged, &target)?,
                    }
                } else {
                    fs::rename(&staged, &target)?;
                }
                meta::set_flags(&target, e.flags).ok();
            }
            OpKind::Symlink => {
                let staged = staging.join(format!("sym-{i}"));
                let _ = fs::remove_file(&staged);
                let t = <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(
                    e.target.as_deref().unwrap_or_default(),
                );
                std::os::unix::fs::symlink(Path::new(t), &staged)?;
                meta::write_xattrs(&staged, &e.xattrs, self.cfg.strip_quarantine)?;
                meta::set_mtime(&staged, Kind::Symlink, e.mtime_ns).ok();
                if let Some(p) = target.parent() {
                    fs::create_dir_all(p)?;
                }
                self.backup(&target, trash, &trash_rel)?;
                fs::rename(&staged, &target)?;
            }
            OpKind::Delete => {
                let Ok(md) = fs::symlink_metadata(&target) else {
                    return Ok(());
                };
                if md.is_dir() {
                    match fs::remove_dir(&target) {
                        Ok(()) => {}
                        Err(_) if self.only_excluded_inside(&target) => {
                            ops::move_to_trash(&target, trash, &trash_rel)?;
                        }
                        // Contains files that are not (yet) synced: keep it.
                        Err(_) => {}
                    }
                } else {
                    ops::move_to_trash(&target, trash, &trash_rel)?;
                }
            }
        }
        Ok(())
    }

    fn set_meta(&self, path: &Path, e: &macsync_core::Entry) -> Result<()> {
        meta::set_mode(path, e.kind, e.mode)?;
        meta::write_xattrs(path, &e.xattrs, self.cfg.strip_quarantine)?;
        Ok(())
    }

    /// Keep the previous version of `target` in the trash before it is
    /// replaced. Uses a hard link (O(1), keeps the old inode) where possible.
    fn backup(&self, target: &Path, trash: &Path, rel: &str) -> Result<()> {
        let Ok(md) = fs::symlink_metadata(target) else {
            return Ok(());
        };
        meta::unlock_for_write(target);
        if md.is_dir() {
            ops::move_to_trash(target, trash, rel)?;
            return Ok(());
        }
        let dest = trash.join(rel);
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }
        if fs::symlink_metadata(&dest).is_ok() {
            return Ok(()); // already backed up (recovery)
        }
        if md.file_type().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(target)?, &dest)?;
        } else if fs::hard_link(target, &dest).is_err() {
            ops::clone_or_copy(target, &dest)?;
        }
        Ok(())
    }

    fn only_excluded_inside(&self, dir: &Path) -> bool {
        let Ok(rd) = fs::read_dir(dir) else {
            return false;
        };
        rd.flatten().all(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let excluded = self.cfg.excludes.is_excluded(&name, &name);
            excluded && !e.path().join(".git").exists()
        })
    }

    /// Write the database rows of an applied plan (one SQLite transaction) and
    /// remove the journal entry.
    fn commit(&self, plan: &Plan, how: &str) -> Result<()> {
        let now = macsync_fs::time::now_ms();
        let unit = plan.unit.as_str();
        let mut rows: Vec<(Option<Stored>, &Op)> = Vec::with_capacity(plan.ops.len());
        for op in &plan.ops {
            let target = macsync_fs::abs_path(&self.cfg.root, unit, &op.disk);
            let row = match op.kind {
                OpKind::Delete => {
                    if fs::symlink_metadata(&target).is_ok() {
                        None // kept (contains unsynced files)
                    } else {
                        Some(Stored {
                            entry: op.entry.clone(),
                            disk: op.disk.clone(),
                            stat: StatCache::default(),
                            changed_ms: now,
                        })
                    }
                }
                OpKind::DbOnly => {
                    let prev = self.store().entry(unit, &op.path)?;
                    let (stat, changed) =
                        prev.as_ref().map(|p| (p.stat, p.changed_ms)).unwrap_or((StatCache::default(), now));
                    let changed =
                        if prev.as_ref().is_some_and(|p| p.entry.deleted != op.entry.deleted) { now } else { changed };
                    Some(Stored { entry: op.entry.clone(), disk: op.disk.clone(), stat, changed_ms: changed })
                }
                _ => {
                    let st = scan::stat(&target, op.path.clone(), op.disk.clone())?;
                    st.map(|s| {
                        let mut entry = op.entry.clone();
                        if s.kind == Kind::Dir {
                            entry.mtime_ns = s.mtime_ns;
                        }
                        Stored {
                            entry,
                            disk: op.disk.clone(),
                            stat: StatCache { ino: s.ino, dev: s.dev, ctime_ns: s.ctime_ns },
                            changed_ms: now,
                        }
                    })
                }
            };
            rows.push((row, op));
        }
        let store = self.store();
        store.atomically(|st| {
            for (row, op) in &rows {
                match row {
                    Some(r) => {
                        st.put_entry(unit, r)?;
                        if st.supersede_conflicts(unit, &op.path)? > 0 {
                            st.log(unit, "resolve", &op.path, "conflict superseded by a newer version", "")?;
                        }
                        if op.kind != OpKind::DbOnly {
                            let action = match op.kind {
                                OpKind::Delete => "delete",
                                OpKind::Meta => "meta",
                                _ => "write",
                            };
                            st.log(unit, action, &op.path, &op.reason, &op.entry.vv.display())?;
                        }
                    }
                    None => st.log(unit, "keep", &op.path, "directory contains unsynced files; not deleted", "")?,
                }
            }
            for c in &plan.conflicts {
                let remote = c.remotes.first().map(|r| r.vv.display()).unwrap_or_default();
                let (id, new) = st.add_conflict(unit, &c.path, &c.class, &c.locals, &c.remotes, &c.dir)?;
                if new {
                    st.log(unit, "conflict", &c.path, &format!("{} (#{id}); local version kept", c.class), &remote)?;
                }
            }
            if how != "applied" {
                st.log(unit, "recovered", "", how, &plan.tx)?;
            }
            st.tx_remove(&plan.tx)?;
            Ok(())
        })?;
        drop(store);
        self.invalidate_tree(unit)?;
        Ok(())
    }

    /// Complete interrupted transactions (journal state `applying`) and clean
    /// up orphaned staging directories. Called on startup.
    pub fn recover(&self) -> Result<usize> {
        let pending = self.store().tx_pending()?;
        let mut done = 0;
        for tx in &pending {
            let staging = self.staging_root().join(&tx.id);
            if tx.state != "applying" {
                let _ = ops::remove_any(&staging);
                self.store().tx_remove(&tx.id)?;
                continue;
            }
            let mut plan: Plan = macsync_core::codec::decode(&tx.plan).context("decode journaled plan")?;
            let lock = self.unit_lock(&plan.unit);
            let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
            let unit_dir = macsync_fs::unit_dir(&self.cfg.root, &plan.unit);
            tracing::warn!(tx = %tx.id, unit = %plan.unit, "completing interrupted transaction");
            // Our own lock may still exist; it is reused (nobody else can hold it).
            if let Some(l) = &plan.lock {
                if fs::symlink_metadata(l).is_err() {
                    let _ = fs::OpenOptions::new().write(true).create_new(true).open(l);
                }
            }
            self.run_ops(&mut plan, &staging, &unit_dir)?;
            self.commit(&plan, "interrupted transaction completed")?;
            let _ = ops::remove_any(&staging);
            if unit_dir.exists() && plan.unit_kind == UnitKind::Git && macsync_gitaware::cmd::available() {
                let _ = macsync_gitaware::cmd::refresh_index(&unit_dir);
            }
            self.scan_unit_locked(&plan.unit, &crate::capture::ScanOptions::default())?;
            done += 1;
        }
        // Orphaned staging directories (no journal entry).
        let known: Vec<String> = self.store().tx_pending()?.into_iter().map(|t| t.id).collect();
        if let Ok(rd) = fs::read_dir(self.staging_root()) {
            for e in rd.flatten() {
                if !known.contains(&e.file_name().to_string_lossy().into_owned()) {
                    let _ = ops::remove_any(&e.path());
                }
            }
        }
        Ok(done)
    }

    /// Put objects deleted by `plan` back from the trash (fsck safety net).
    pub(crate) fn restore_object_deletions(&self, plan: &Plan) -> Result<usize> {
        let trash = self.trash_root().join(&plan.tx);
        let mut n = 0;
        for op in plan.ops.iter().filter(|o| o.phase == Phase::ObjectDeletes) {
            let from: PathBuf = trash.join(macsync_fs::root_rel(&plan.unit, &op.disk));
            let to = macsync_fs::abs_path(&self.cfg.root, &plan.unit, &op.disk);
            if fs::symlink_metadata(&from).is_ok() && fs::symlink_metadata(&to).is_err() {
                if let Some(p) = to.parent() {
                    fs::create_dir_all(p)?;
                }
                fs::rename(&from, &to)?;
                n += 1;
            }
        }
        Ok(n)
    }
}
