//! Resolving recorded conflicts: keep the local version, take the remote
//! version from the conflict store, or keep both.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Context, Result};
use macsync_core::{path as rpath, ConflictClass, Entry, Hash, Kind};
use macsync_fs::ops;
use macsync_store::{ConflictRow, Stored};
use serde::Serialize;

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
        self.resolve_many(&[id], choice)?.remove(0).1
    }

    /// Resolve several conflicts with the same choice, one result per id (in
    /// the given order). Each unit is scanned once before and once after its
    /// conflicts, not per conflict – hundreds of conflicts in one large unit
    /// are resolved in one go.
    pub fn resolve_many(&self, ids: &[i64], choice: Choice) -> Result<Vec<(i64, Result<String>)>> {
        let mut out: Vec<(i64, Option<Result<String>>)> = ids.iter().map(|&id| (id, None)).collect();
        let mut by_unit: BTreeMap<String, Vec<(usize, ConflictRow)>> = BTreeMap::new();
        for (i, &id) in ids.iter().enumerate() {
            match self.store().conflict(id)? {
                None => out[i].1 = Some(Err(anyhow!("no conflict #{id}"))),
                Some(c) => match (&c.resolution, c.resolved_ms) {
                    (r, Some(_)) => {
                        out[i].1 =
                            Some(Err(anyhow!("conflict #{id} is already resolved ({})", r.clone().unwrap_or_default())))
                    }
                    _ => by_unit.entry(c.unit.clone()).or_default().push((i, c)),
                },
            }
        }
        for (unit, rows) in by_unit {
            let lock = self.unit_lock(&unit);
            let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
            // Make sure the index reflects the disk before deciding anything –
            // for the paths in conflict only; a case collision involves other
            // spellings, so the whole unit is scanned.
            let whole = rows.iter().any(|(_, c)| ConflictClass::parse(&c.class) == Some(ConflictClass::CaseCollision));
            let mut paths: Vec<String> = rows
                .iter()
                .flat_map(|(_, c)| {
                    std::iter::once(c.path.clone())
                        .chain(c.locals.iter().map(|e| e.path.clone()))
                        .chain(c.remotes.iter().map(|e| e.path.clone()))
                })
                .collect();
            let capture = |paths: &[String]| {
                if whole {
                    self.scan_unit_locked(&unit, &ScanOptions::default())
                } else {
                    self.capture_paths_locked(&unit, paths)
                }
            };
            let s = capture(&paths)?;
            if let Some(d) = s.deferred {
                for (i, _) in rows {
                    out[i].1 = Some(Err(anyhow!("unit {unit} is busy: {d}")));
                }
                continue;
            }
            for (i, c) in rows {
                let r = self.resolve_locked(&c, choice).and_then(|added| {
                    paths.extend(added);
                    self.store().resolve_conflict(c.id, choice.as_str())?;
                    self.store().log(
                        &unit,
                        "resolve",
                        &c.path,
                        &format!("conflict #{} resolved: {}", c.id, choice.as_str()),
                        "",
                    )?;
                    Ok(format!("conflict #{} ({} {}) resolved: {}", c.id, c.class, c.path, choice.as_str()))
                });
                out[i].1 = Some(r);
            }
            // Git rewrites its index after a remote version was taken.
            if self.store().unit(&unit)?.is_some_and(|u| u.kind == macsync_core::UnitKind::Git) {
                paths.push(".git/index".into());
            }
            capture(&paths)?;
            self.invalidate_tree(&unit)?;
            let open = self.store().open_conflict_count(&unit)?;
            self.store().set_unit_status(&unit, if open > 0 { "conflict" } else { "resolved" }, false)?;
        }
        Ok(out
            .into_iter()
            .map(|(id, r)| (id, r.unwrap_or_else(|| Err(anyhow!("conflict #{id} not processed")))))
            .collect())
    }

    /// Apply `choice` to one open conflict; the unit lock is held and the
    /// index is current. Returns paths it created besides the conflict's own
    /// (the sibling file of "keep both"); the caller recomputes the tree.
    fn resolve_locked(&self, c: &ConflictRow, choice: Choice) -> Result<Vec<String>> {
        let id = c.id;
        let unit = c.unit.clone();
        let me = self.cfg.device.clone();
        let class = ConflictClass::parse(&c.class);

        match choice {
            Choice::Local | Choice::Both => {
                if choice == Choice::Both && !keeps_both(c) {
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
                                    s.changed_ms = macsync_fs::time::now_ms();
                                    s
                                }
                                None => Stored {
                                    entry: Entry::tombstone(&r.path, r.kind, r.vv.merge_bump(&r.vv, &me)),
                                    disk: r.path.clone(),
                                    stat: Default::default(),
                                    changed_ms: macsync_fs::time::now_ms(),
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
                    let dest = macsync_fs::abs_path(&self.cfg.root, &unit, &sibling);
                    if let Some(p) = dest.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    ops::clone_or_copy(&src, &dest).with_context(|| format!("copy {}", src.display()))?;
                    return Ok(vec![sibling]);
                }
            }
            Choice::Remote => {
                let staging = self.staging_root().join(format!("resolve-{id}-{}", crate::new_tx_id()));
                std::fs::create_dir_all(&staging)?;
                let r = self.apply_conflict_remote(c, &staging);
                if self.store().tx_pending()?.iter().all(|t| !staging.ends_with(&t.id)) {
                    let _ = ops::remove_any(&staging);
                }
                r?;
            }
        }
        Ok(Vec::new())
    }

    /// Conflicts with both versions side by side (the app's conflict view).
    pub fn conflict_views(&self, include_resolved: bool) -> Result<Vec<ConflictView>> {
        let rows = self.store().conflicts(include_resolved)?;
        let mut out = Vec::with_capacity(rows.len());
        for c in rows {
            // Case collisions: the local entry is spelled differently.
            let ck = rpath::case_key(&c.path);
            let local_path = c
                .locals
                .iter()
                .find(|l| l.path == c.path)
                .or_else(|| c.locals.iter().find(|l| rpath::case_key(&l.path) == ck))
                .map(|l| l.path.clone())
                .unwrap_or_else(|| c.path.clone());
            let local = match self.store().entry(&c.unit, &local_path)? {
                Some(s) => {
                    let file = macsync_fs::abs_path(&self.cfg.root, &c.unit, &s.disk);
                    Side::of(&s.entry, file.exists().then_some(file))
                }
                None => Side::missing(),
            };
            let remote = match c.remotes.iter().find(|r| r.path == c.path).or(c.remotes.first()) {
                Some(r) => {
                    let file =
                        std::path::Path::new(&c.stored).join(if r.path.is_empty() { "_unit_root" } else { &r.path });
                    Side::of(r, (!r.deleted && file.exists()).then_some(file))
                }
                None => Side::missing(),
            };
            out.push(ConflictView {
                id: c.id,
                unit: c.unit.clone(),
                path: c.path.clone(),
                local_path,
                class: c.class.clone(),
                created_ms: c.created_ms,
                resolved_ms: c.resolved_ms,
                resolution: c.resolution.clone(),
                paths: c.remotes.len().max(c.locals.len()),
                can_keep_both: keeps_both(&c),
                local,
                remote,
            });
        }
        Ok(out)
    }

    fn apply_conflict_remote(&self, c: &ConflictRow, staging: &std::path::Path) -> Result<()> {
        let unit = c.unit.as_str();
        let kind = self.store().unit(unit)?.map(|u| u.kind).unwrap_or(macsync_core::UnitKind::Folder);
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
        let case_collision = ConflictClass::parse(&c.class) == Some(ConflictClass::CaseCollision);
        for r in &c.remotes {
            let mut local = self.store().entry(unit, &r.path)?;
            let class = class_of(kind, &r.path);
            // Case collision: the local version is spelled differently. Taking
            // the remote version takes its spelling too – rename in place and
            // retire the old spelling (the peer follows the rename).
            let mut respelled = None;
            if case_collision && self.case_insensitive && local.is_none() && !r.deleted {
                let ck = rpath::case_key(&r.path);
                for l in c.locals.iter().filter(|l| l.path != r.path && rpath::case_key(&l.path) == ck) {
                    if let Some(s) =
                        self.store().entry(unit, &l.path)?.filter(|s| !s.entry.deleted && s.entry.kind == r.kind)
                    {
                        respelled = Some(s);
                        break;
                    }
                }
            }
            let mut disk = local.as_ref().map(|l| l.disk.clone()).unwrap_or_else(|| r.path.clone());
            if let Some(s) = &respelled {
                disk = rpath::join(rpath::parent(&s.disk).unwrap_or(""), rpath::file_name(&r.path));
                plan.ops.push(Op {
                    path: s.entry.path.clone(),
                    disk: s.disk.clone(),
                    kind: OpKind::DbOnly,
                    phase: Phase::Db,
                    rank: rank_of(class),
                    entry: Entry::tombstone(&s.entry.path, s.entry.kind, s.entry.vv.merge_bump(&r.vv, &me)),
                    staged: None,
                    source: None,
                    reason: format!("renamed to {} (conflict resolved)", r.path),
                });
                plan.ops.push(Op {
                    path: r.path.clone(),
                    disk: disk.clone(),
                    kind: OpKind::Rename,
                    phase: Phase::Dirs,
                    rank: rank_of(class),
                    entry: Entry { vv: r.vv.merge_bump(&s.entry.vv, &me), ..r.clone() },
                    staged: None,
                    source: Some(Source::Rename { from: s.disk.clone() }),
                    reason: format!("conflict resolved: spelling {:?} taken", r.path),
                });
                local = Some(Stored { entry: Entry { path: r.path.clone(), ..s.entry.clone() }, ..s.clone() });
            }
            let mut e = r.clone();
            e.vv = match &local {
                Some(l) => l.entry.vv.merge_bump(&r.vv, &me),
                None => r.vv.merge_bump(&r.vv, &me),
            };
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
        if kind == macsync_core::UnitKind::Git {
            let g = self.gate.check(&macsync_fs::unit_dir(&self.cfg.root, unit));
            if !g.is_open() {
                bail!("unit {unit} is busy: {}", g.describe());
            }
        }
        self.execute(&mut plan, staging)?;
        let unit_dir = macsync_fs::unit_dir(&self.cfg.root, unit);
        if kind == macsync_core::UnitKind::Git && macsync_gitaware::cmd::available() {
            let _ = macsync_gitaware::cmd::refresh_index(&unit_dir);
        }
        Ok(())
    }
}

/// A conflict as shown in the app.
#[derive(Clone, Debug, Serialize)]
pub struct ConflictView {
    pub id: i64,
    pub unit: String,
    /// Path of the remote version.
    pub path: String,
    /// Path of the local version (differs only for case collisions).
    pub local_path: String,
    pub class: String,
    pub created_ms: i64,
    pub resolved_ms: Option<i64>,
    pub resolution: Option<String>,
    /// Number of paths held back (the whole Git state for `git-state`).
    pub paths: usize,
    pub can_keep_both: bool,
    /// The version on this Mac now.
    pub local: Side,
    /// The version of the other Mac that was held back.
    pub remote: Side,
}

/// One version of a conflicting path.
#[derive(Clone, Debug, Serialize)]
pub struct Side {
    /// `present`, `deleted` or `missing`.
    pub state: &'static str,
    pub kind: &'static str,
    pub size: Option<u64>,
    pub mtime_ms: Option<i64>,
    /// Where this version can be opened on this Mac (the file itself, or its
    /// copy in the conflict store).
    pub file: Option<String>,
}

impl Side {
    fn of(e: &Entry, file: Option<std::path::PathBuf>) -> Side {
        let kind = match e.kind {
            Kind::File => "file",
            Kind::Dir => "dir",
            Kind::Symlink => "symlink",
        };
        if e.deleted {
            return Side { state: "deleted", kind, size: None, mtime_ms: None, file: None };
        }
        Side {
            state: "present",
            kind,
            size: (e.kind == Kind::File).then_some(e.size),
            mtime_ms: (e.mtime_ns != 0).then_some(e.mtime_ns / 1_000_000),
            file: file.map(|f| f.to_string_lossy().into_owned()),
        }
    }

    fn missing() -> Side {
        Side { state: "missing", kind: "file", size: None, mtime_ms: None, file: None }
    }
}

/// "Keep both" adds the remote version as a sibling file: one live file only.
fn keeps_both(c: &ConflictRow) -> bool {
    c.remotes.len() == 1 && c.remotes[0].kind == Kind::File && !c.remotes[0].deleted
}

/// `dir/name.ext` → `dir/name.conflict-<device>.ext`.
fn sibling_name(path: &str, device: &str) -> String {
    let dir = rpath::parent(path).unwrap_or("");
    let name = rpath::file_name(path);
    let dev = &device[..device.len().min(8)];
    let ts = macsync_fs::time::compact_utc(macsync_fs::time::now_ms() / 1000);
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
