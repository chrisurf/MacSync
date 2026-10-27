//! Capturing local state: scan a unit, hash what changed, bump version vectors,
//! write tombstones.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use anyhow::Result;
use macsync_core::entry::ROOT_UNIT;
use macsync_core::{gitpath, path as rpath, Entry, Hash, Kind, UnitKind, VersionVector};
use macsync_fs::{meta, ops, scan, Scanned};
use macsync_gitaware::GateState;
use macsync_store::{StatCache, Stored};
use serde::Serialize;

use crate::Engine;

#[derive(Debug, Default, Clone, Serialize)]
pub struct ScanReport {
    pub unit: String,
    /// Entries whose state changed (incl. new tombstones).
    pub changed: usize,
    /// Files that had to be hashed.
    pub hashed: usize,
    pub root: Hash,
    pub nested: Vec<String>,
    /// Set if the scan was not committed (gate closed or unit not quiet).
    pub deferred: Option<String>,
    /// Content changes found without a stat change (only in `full` mode).
    pub silent_changes: usize,
}

#[derive(Default)]
pub struct ScanOptions<'a> {
    /// Re-hash every file, ignoring the stat cache (daily verification).
    pub full: bool,
    /// Re-checked after the scan; if it returns false the scan is discarded.
    pub still_quiet: Option<&'a dyn Fn() -> bool>,
}

impl Engine {
    /// Capture the local state of one unit into the index.
    pub fn scan_unit(&self, unit: &str, opts: &ScanOptions<'_>) -> Result<ScanReport> {
        crate::validate_unit_name(unit)?;
        let lock = self.unit_lock(unit);
        let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
        self.scan_unit_locked(unit, opts)
    }

    pub(crate) fn scan_unit_locked(&self, unit: &str, opts: &ScanOptions<'_>) -> Result<ScanReport> {
        let root = &self.cfg.root;
        let known = self.store().unit(unit)?;
        let kind = scan::unit_kind_on_disk(root, unit).or(known.as_ref().map(|u| u.kind)).unwrap_or(UnitKind::Folder);
        if known.as_ref().is_none_or(|u| u.kind != kind) {
            self.store().upsert_unit(unit, kind)?;
        }
        let mut report = ScanReport { unit: unit.to_string(), ..Default::default() };
        let unit_dir = macsync_fs::unit_dir(root, unit);

        if kind == UnitKind::Git {
            let g = self.gate.check(&unit_dir);
            if !g.is_open() {
                report.deferred = Some(g.describe());
                report.root = known.and_then(|u| u.root).unwrap_or_default();
                return Ok(report);
            }
        }

        let res = scan::scan_unit(root, unit, kind, &self.cfg.excludes)?;
        let nested_known: Vec<String> = self.nested_units_of(unit)?;
        let nested_rel: Vec<String> = nested_known
            .iter()
            .filter_map(|n| n.strip_prefix(&format!("{unit}/")).map(str::to_string))
            .chain(res.nested_units.iter().cloned())
            .collect();
        let in_nested = |p: &str| nested_rel.iter().any(|n| rpath::is_within(p, n));

        for (p, reason) in &res.skipped {
            self.log_skip_once(unit, p, reason);
        }

        let prev: HashMap<String, Stored> =
            self.store().entries(unit)?.into_iter().map(|s| (s.entry.path.clone(), s)).collect();

        // Hard link groups: leader = lexicographically smallest path.
        let mut groups: BTreeMap<(u64, u64), Vec<&str>> = BTreeMap::new();
        for s in &res.items {
            if s.kind == Kind::File && s.nlink > 1 {
                groups.entry((s.dev, s.ino)).or_default().push(&s.path);
            }
        }
        let mut leader_of: HashMap<&str, String> = HashMap::new();
        for paths in groups.values() {
            if paths.len() > 1 {
                let leader = *paths.iter().min().expect("non-empty");
                for p in paths {
                    if *p != leader {
                        leader_of.insert(p, leader.to_string());
                    }
                }
            }
        }

        let oid_len = if kind == UnitKind::Git {
            macsync_gitaware::resolve_git_dir(&unit_dir).map(|g| macsync_gitaware::object_id_len(&g)).unwrap_or(20)
        } else {
            20
        };
        let now = macsync_fs::time::now_ms();
        let me = self.cfg.device.as_str();
        let mut writes: Vec<Stored> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();

        for s in &res.items {
            if in_nested(&s.path) {
                continue;
            }
            seen.insert(&s.path);
            let old = prev.get(&s.path);
            let link = leader_of.get(s.path.as_str()).cloned();
            let Some((entry, hashed)) = self.build_entry(unit, kind, oid_len, s, old, opts.full, link)? else {
                continue; // vanished while scanning
            };
            if hashed {
                report.hashed += 1;
            }
            let stat = StatCache { ino: s.ino, dev: s.dev, ctime_ns: s.ctime_ns };
            let stored = match old {
                None => Some(Stored {
                    entry: Entry { vv: VersionVector::single(me, 1), ..entry },
                    disk: s.disk.clone(),
                    stat,
                    changed_ms: now,
                }),
                Some(o) if o.entry.deleted || !o.entry.same_state(&entry) => {
                    if opts.full && !o.entry.deleted && stat_unchanged(o, s) {
                        report.silent_changes += 1;
                    }
                    let mut vv = o.entry.vv.clone();
                    vv.increment(me);
                    Some(Stored { entry: Entry { vv, ..entry }, disk: s.disk.clone(), stat, changed_ms: now })
                }
                Some(o) => {
                    // Same state. Refresh stat cache, bytes hash (index) and mtime silently.
                    let e = Entry { vv: o.entry.vv.clone(), ..entry };
                    if e != o.entry || o.disk != s.disk || o.stat != stat {
                        writes.push(Stored { entry: e, disk: s.disk.clone(), stat, changed_ms: o.changed_ms });
                    }
                    None
                }
            };
            if let Some(st) = stored {
                report.changed += 1;
                writes.push(st);
            }
        }

        let mut forget: Vec<String> = Vec::new();
        for (p, o) in &prev {
            if seen.contains(p.as_str()) || o.entry.deleted {
                continue;
            }
            let excluded = self.cfg.excludes.is_excluded(&macsync_fs::root_rel(unit, p), rpath::file_name(p))
                || (kind == UnitKind::Git && gitpath::is_excluded(p));
            if excluded || in_nested(p) {
                forget.push(p.clone());
                continue;
            }
            let mut vv = o.entry.vv.clone();
            vv.increment(me);
            report.changed += 1;
            writes.push(Stored {
                entry: Entry::tombstone(p, o.entry.kind, vv),
                disk: o.disk.clone(),
                stat: StatCache::default(),
                changed_ms: now,
            });
        }

        // Optimistic snapshot (ADR-0005): discard the scan if anything moved.
        if let Some(q) = opts.still_quiet {
            if !q() {
                report.deferred = Some("unit changed during scan".into());
                return Ok(report);
            }
        }
        if kind == UnitKind::Git {
            let g = macsync_gitaware::Gate::check_files(&unit_dir);
            if g != GateState::Open {
                report.deferred = Some(g.describe());
                return Ok(report);
            }
        }

        {
            let store = self.store();
            store.atomically(|st| {
                for w in &writes {
                    st.put_entry(unit, w)?;
                }
                for p in &forget {
                    st.remove_entry(unit, p)?;
                }
                if report.changed > 0 {
                    st.log(unit, "capture", "", "local change", &format!("{} entries changed", report.changed))?;
                }
                if report.silent_changes > 0 {
                    st.log(
                        unit,
                        "drift",
                        "",
                        "verify found content changes without stat change",
                        &report.silent_changes.to_string(),
                    )?;
                }
                Ok(())
            })?;
        }
        report.root = self.invalidate_tree(unit)?;
        report.nested = res.nested_units.iter().map(|n| macsync_fs::root_rel(unit, n)).collect();
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_entry(
        &self,
        unit: &str,
        kind: UnitKind,
        oid_len: usize,
        s: &Scanned,
        old: Option<&Stored>,
        full: bool,
        link: Option<String>,
    ) -> Result<Option<(Entry, bool)>> {
        let abs = macsync_fs::abs_path(&self.cfg.root, unit, &s.disk);
        if let Some(o) = old.filter(|o| !full && !o.entry.deleted && stat_unchanged(o, s)) {
            let mut e = o.entry.clone();
            e.link = link;
            e.flags = s.flags;
            e.target = s.target.clone();
            return Ok(Some((e, false)));
        }
        let (blob, cmp, hashed) = if s.kind == Kind::File {
            let blob = match ops::hash_file(&abs) {
                Ok(h) => h,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => {
                    self.log_skip_once(unit, &s.path, &format!("unreadable: {e}"));
                    return Ok(None);
                }
            };
            let cmp = if kind == UnitKind::Git && s.path == ".git/index" {
                std::fs::read(&abs)
                    .ok()
                    .and_then(|d| macsync_gitaware::index::semantic_hash(&d, oid_len))
                    .unwrap_or(blob)
            } else {
                blob
            };
            (Some(blob), Some(cmp), true)
        } else {
            (None, None, false)
        };
        Ok(Some((
            Entry {
                path: s.path.clone(),
                kind: s.kind,
                mode: s.mode,
                size: s.size,
                mtime_ns: s.mtime_ns,
                blob,
                cmp,
                target: s.target.clone(),
                xattrs: meta::read_xattrs(&abs),
                flags: s.flags,
                link,
                deleted: false,
                vv: VersionVector::new(),
            },
            hashed,
        )))
    }

    fn log_skip_once(&self, unit: &str, path: &str, reason: &str) {
        static LOGGED: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);
        let key = format!("{}\0{unit}\0{path}", self.cfg.state_dir.display());
        let mut g = LOGGED.lock().unwrap_or_else(|e| e.into_inner());
        if g.get_or_insert_with(HashSet::new).insert(key) {
            tracing::info!(unit, path, reason, "skipped");
            let _ = self.store().log(unit, "skip", path, reason, "");
        }
    }

    /// Discover all units (top level and nested) and capture each of them.
    /// Units whose directory vanished are scanned too (their entries become
    /// tombstones); they are never dropped silently.
    pub fn scan_all(&self, full: bool) -> Result<Vec<ScanReport>> {
        let root = &self.cfg.root;
        self.reconcile_unit_spelling()?;
        for (name, kind) in scan::top_level_units(root, &self.cfg.excludes)? {
            self.ensure_unit(&name, kind)?;
        }
        let mut queue: VecDeque<String> = self.store().units()?.into_iter().map(|u| u.name).collect();
        let mut done: HashSet<String> = HashSet::new();
        let mut reports = Vec::new();
        while let Some(u) = queue.pop_front() {
            if !done.insert(u.clone()) {
                continue;
            }
            let r = self.scan_unit(&u, &ScanOptions { full, still_quiet: None })?;
            for n in &r.nested {
                if let Some(k) = scan::unit_kind_on_disk(root, n) {
                    self.ensure_unit(n, k)?;
                    queue.push_back(n.clone());
                }
            }
            reports.push(r);
        }
        Ok(reports)
    }

    /// Full verification of one unit against the file system (re-hash all).
    pub fn verify(&self, unit: &str) -> Result<ScanReport> {
        self.scan_unit(unit, &ScanOptions { full: true, still_quiet: None })
    }

    /// Unit owning a root-relative path (longest matching unit name).
    pub fn unit_for_path(&self, root_rel: &str) -> Result<Option<String>> {
        if root_rel.is_empty() {
            return Ok(None);
        }
        let units = self.store().units()?;
        let best = units
            .iter()
            .filter(|u| u.name != ROOT_UNIT && rpath::is_within(root_rel, &u.name))
            .max_by_key(|u| u.name.len())
            .map(|u| u.name.clone());
        Ok(best.or_else(|| {
            // Loose file in the root, or a new top-level directory.
            let first = root_rel.split('/').next().unwrap_or("");
            if root_rel.contains('/') || self.cfg.root.join(first).is_dir() {
                Some(first.to_string())
            } else {
                Some(ROOT_UNIT.to_string())
            }
        }))
    }
}

fn stat_unchanged(o: &Stored, s: &Scanned) -> bool {
    o.entry.kind == s.kind
        && o.entry.size == s.size
        && o.entry.mtime_ns == s.mtime_ns
        && o.entry.mode == s.mode
        && o.stat.ino == s.ino
        && o.stat.dev == s.dev
        && o.stat.ctime_ns == s.ctime_ns
}
