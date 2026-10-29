//! Pulling a unit from a peer: Merkle descent, decisions, Git coherence,
//! staging with hash verification, pre-apply checks.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::Result;
use macsync_core::proto::{limits, UnitRoot};
use macsync_core::{
    decide, path as rpath, ConflictClass, Decision, Entry, GitClass, Hash, Kind, Policy, Tree, UnitKind,
};
use macsync_fs::{ops, scan};
use macsync_store::Stored;
use serde::Serialize;

use crate::capture::ScanOptions;
use crate::plan::{class_of, rank_of, ActionKind, ConflictPlan, Op, OpKind, Phase, Plan, PlannedAction, Source};
use crate::{Engine, Mode, PeerApi};

#[derive(Clone, Copy, Debug, Default)]
pub struct PullOptions {
    /// Compute and report the plan without changing anything.
    pub dry_run: bool,
}

/// Units the user wants first (the daemon's priority queue).
pub trait PullQueue {
    /// The next unit to pull ahead of the rest, skipping `done`.
    fn next(&self, done: &BTreeSet<String>) -> Option<String>;
    /// `unit` was pulled (or does not exist on the peer).
    fn done(&self, unit: &str);
}

struct NoQueue;

impl PullQueue for NoQueue {
    fn next(&self, _: &BTreeSet<String>) -> Option<String> {
        None
    }
    fn done(&self, _: &str) {}
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Outcome {
    InSync,
    Applied,
    DryRun,
    /// Not possible right now (gate closed, unscanned local change, source
    /// changed during transfer); retried automatically.
    Deferred(String),
    Paused,
    Failed(String),
    /// The name is another spelling of a local unit (case-insensitive volume).
    Skipped(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct PullReport {
    pub unit: String,
    pub outcome: Outcome,
    pub actions: Vec<PlannedAction>,
    pub writes: usize,
    pub deletes: usize,
    pub conflicts: usize,
    pub bytes_fetched: u64,
    pub bytes_deduped: u64,
    pub local_root: Hash,
    pub remote_root: Hash,
    /// True if the local root equals the remote root after the pull.
    pub converged: bool,
    /// Wall-clock milliseconds per phase (diagnostics).
    pub timings_ms: Vec<(String, u64)>,
}

impl PullReport {
    fn lap(&mut self, phase: &str, t: &mut std::time::Instant) {
        self.timings_ms.push((phase.to_string(), t.elapsed().as_millis() as u64));
        *t = std::time::Instant::now();
    }

    fn new(unit: &str, remote_root: Hash) -> PullReport {
        PullReport {
            unit: unit.to_string(),
            outcome: Outcome::InSync,
            actions: Vec::new(),
            writes: 0,
            deletes: 0,
            conflicts: 0,
            bytes_fetched: 0,
            bytes_deduped: 0,
            local_root: Hash::ZERO,
            remote_root,
            converged: false,
            timings_ms: Vec::new(),
        }
    }
}

/// The transaction cannot proceed now; it is retried later.
#[derive(Debug)]
pub struct RetryLater(pub String);

impl std::fmt::Display for RetryLater {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "retry later: {}", self.0)
    }
}

impl std::error::Error for RetryLater {}

fn retry<T>(msg: impl Into<String>) -> Result<T> {
    Err(RetryLater(msg.into()).into())
}

struct Need {
    size: u64,
    dests: Vec<PathBuf>,
    /// Unit-relative path, for progress display.
    label: String,
}

/// Clears the transfer progress when the fetch ends, also on errors.
struct TransferGuard<'a>(&'a Engine);

impl Drop for TransferGuard<'_> {
    fn drop(&mut self) {
        self.0.update_transfer(|t| *t = None);
    }
}

impl Engine {
    /// Pull every unit that differs from the peer (or only `only`).
    pub fn sync_from(&self, peer: &dyn PeerApi, only: Option<&[String]>, opts: PullOptions) -> Result<Vec<PullReport>> {
        self.sync_from_queue(peer, only, opts, &NoQueue)
    }

    /// Like [`Engine::sync_from`], but before each unit `queue` may put a
    /// unit first (also one outside `only`). The running unit always
    /// finishes; units are never pulled twice in one call.
    pub fn sync_from_queue(
        &self,
        peer: &dyn PeerApi,
        only: Option<&[String]>,
        opts: PullOptions,
        queue: &dyn PullQueue,
    ) -> Result<Vec<PullReport>> {
        if self.cfg.mode == Mode::MirrorSend {
            return Ok(Vec::new());
        }
        let mut remote = peer.roots()?;
        remote.retain(|r| crate::validate_unit_name(&r.name).is_ok());
        remote.sort_by(|a, b| a.name.cmp(&b.name));
        let names: BTreeSet<String> = remote.iter().map(|r| r.name.clone()).collect();
        let mut todo: VecDeque<&UnitRoot> =
            remote.iter().filter(|r| only.is_none_or(|o| o.contains(&r.name))).collect();
        let mut done: BTreeSet<String> = BTreeSet::new();
        let mut reports = Vec::new();
        loop {
            if let Some(first) = queue.next(&done) {
                done.insert(first.clone());
                todo.retain(|r| r.name != first);
                // Fresh root: the list above may be old after a long round.
                let fresh = peer.roots()?.into_iter().find(|r| r.name == first);
                if let Some(r) = fresh.filter(|r| crate::validate_unit_name(&r.name).is_ok()) {
                    let rep = self.pull_with_names(peer, &r, &names, opts);
                    queue.done(&first);
                    reports.push(rep);
                } else {
                    queue.done(&first);
                }
                continue;
            }
            let Some(r) = todo.pop_front() else { break };
            done.insert(r.name.clone());
            reports.push(self.pull_with_names(peer, r, &names, opts));
        }
        Ok(reports)
    }

    /// Pull a single unit given the peer's root for it.
    pub fn pull_unit(&self, peer: &dyn PeerApi, remote: &UnitRoot, opts: PullOptions) -> PullReport {
        let names: BTreeSet<String> = [remote.name.clone()].into();
        self.pull_with_names(peer, remote, &names, opts)
    }

    fn pull_with_names(
        &self,
        peer: &dyn PeerApi,
        remote: &UnitRoot,
        names: &BTreeSet<String>,
        opts: PullOptions,
    ) -> PullReport {
        let lock = self.unit_lock(&remote.name);
        let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
        match self.pull_locked(peer, remote, names, opts) {
            Ok(r) => r,
            Err(e) => {
                let mut r = PullReport::new(&remote.name, remote.root);
                if let Some(rl) = e.downcast_ref::<RetryLater>() {
                    r.outcome = Outcome::Deferred(rl.0.clone());
                    let _ = self.store().set_unit_status(&remote.name, &format!("waiting: {}", rl.0), false);
                } else {
                    tracing::warn!(unit = %remote.name, error = %format!("{e:#}"), "pull failed");
                    let _ = self.store().log(&remote.name, "error", "", "pull failed", &format!("{e:#}"));
                    r.outcome = Outcome::Failed(format!("{e:#}"));
                }
                r
            }
        }
    }

    fn pull_locked(
        &self,
        peer: &dyn PeerApi,
        remote: &UnitRoot,
        remote_names: &BTreeSet<String>,
        opts: PullOptions,
    ) -> Result<PullReport> {
        let unit = remote.name.as_str();
        crate::validate_unit_name(unit)?;
        let root = self.cfg.root.clone();
        let mut rep = PullReport::new(unit, remote.root);
        if !opts.dry_run {
            if let Some(why) = self.respell_for_pull(unit, remote_names)? {
                rep.outcome = Outcome::Skipped(why);
                return Ok(rep);
            }
        }
        let row = self.store().unit(unit)?;
        if row.as_ref().is_some_and(|u| u.paused) {
            rep.outcome = Outcome::Paused;
            return Ok(rep);
        }
        let on_disk = scan::unit_kind_on_disk(&root, unit);
        if !opts.dry_run {
            match &row {
                None => self.store().upsert_unit(unit, on_disk.unwrap_or(remote.kind))?,
                Some(u) if u.root.is_none() && on_disk.is_some() => {
                    self.scan_unit_locked(unit, &ScanOptions::default())?;
                }
                _ => {}
            }
        }
        let local_kind = on_disk.or(row.as_ref().map(|u| u.kind)).unwrap_or(remote.kind);
        let kind = if remote.kind == UnitKind::Git || local_kind == UnitKind::Git { UnitKind::Git } else { local_kind };

        let tree = self.tree(unit)?;
        rep.local_root = tree.root();
        if tree.root() == remote.root {
            rep.converged = true;
            if !opts.dry_run {
                self.settle_conflicts(peer, unit)?;
                self.after_converged(unit)?;
            }
            return Ok(rep);
        }
        let unit_dir = macsync_fs::unit_dir(&root, unit);
        if kind == UnitKind::Git && unit_dir.exists() {
            let g = self.gate.check(&unit_dir);
            if !g.is_open() {
                return retry(g.describe());
            }
        }

        // 1. Merkle descent and entry exchange.
        let mut t = std::time::Instant::now();
        let paths = descend(peer, unit, &tree, remote.root)?;
        let mut remote_entries = fetch_entries(peer, unit, &paths)?;
        for p in drop_below_symlinks(&mut remote_entries) {
            if !opts.dry_run {
                let _ = self.store().log(unit, "warning", &p, "ignored: the peer sent it below a symbolic link", "");
            }
        }
        let mut local: HashMap<String, Stored> = HashMap::new();
        let wanted: Vec<&String> = paths.iter().collect();
        for chunk in wanted.chunks(limits::ENTRY_BATCH) {
            // In chunks: status and the peer's requests need the store too.
            let store = self.store();
            for p in chunk {
                if let Some(s) = store.entry(unit, p)? {
                    local.insert((*p).clone(), s);
                }
            }
        }
        let keys: BTreeSet<String> = remote_entries.keys().chain(local.keys()).cloned().collect();

        rep.lap("descend", &mut t);
        // 2. Decisions.
        let policy = self.policy_for(unit);
        let me = self.cfg.device.clone();
        let mut decisions: Vec<(String, Option<GitClass>, Decision)> = Vec::new();
        for key in &keys {
            let class = class_of(kind, key);
            if class == Some(GitClass::ProcessLocal) {
                continue;
            }
            let l = local.get(key).map(|s| &s.entry);
            let d = match remote_entries.get(key) {
                Some(r) => decide(l, r, class, policy, &me),
                None if policy == Policy::MirrorReceive && l.is_some_and(|l| !l.deleted) => {
                    let l = l.expect("checked");
                    Decision::TakeRemote(l.vv.clone())
                }
                None => continue,
            };
            decisions.push((key.clone(), class, d));
        }

        // 3. Git coherence: a conflict in HEAD/refs/index holds back the whole
        //    Git state of the unit as one conflict.
        let git_state_conflict =
            decisions.iter().any(|(_, c, d)| c.is_some_and(|c| c.is_git_state()) && matches!(d, Decision::Conflict(_)));
        let nested: Vec<String> = {
            let mut n: BTreeSet<String> = self.nested_units_of(unit)?.into_iter().collect();
            n.extend(remote_names.iter().filter(|r| r.starts_with(&format!("{unit}/"))).cloned());
            if unit == macsync_core::entry::ROOT_UNIT {
                n.clear();
            }
            n.into_iter().filter_map(|u| u.strip_prefix(&format!("{unit}/")).map(str::to_string)).collect()
        };

        // 4. Spelling-only renames (case-insensitive volume): the peer renamed
        //    `old` to `new`, which name the same file here. `new` takes over
        //    the local entry of `old` and the file is renamed in place – never
        //    deleted and fetched again, never reported as a case collision.
        let mut respelled: HashMap<String, String> = HashMap::new();
        let mut respell_sources: HashSet<String> = HashSet::new();
        if self.case_insensitive {
            for (new, old) in case_renames(&decisions, &local, &remote_entries, &nested) {
                let o = local[&old].clone();
                let mut s = o.clone();
                s.entry.path = new.clone();
                s.disk = respell_disk(&o.disk, &old, &new);
                if let Some(t) = local.get(&new) {
                    s.entry.vv = s.entry.vv.merge(&t.entry.vv);
                }
                // The peer's tombstone of `old` superseded our state of it
                // (otherwise `old` would not be a rename source), so the
                // peer's `new` is the successor of our entry, even though its
                // version vector starts afresh under the new name.
                let r = &remote_entries[&new];
                let vv = s.entry.vv.merge(&r.vv);
                let d = if !s.entry.same_state(r) {
                    Decision::TakeRemote(vv)
                } else if s.entry.vv == r.vv {
                    Decision::Nothing
                } else {
                    Decision::AdoptVv(vv)
                };
                if let Some(slot) = decisions.iter_mut().find(|(p, _, _)| *p == new) {
                    slot.2 = d;
                }
                local.insert(new.clone(), s);
                respelled.insert(new, o.disk);
                respell_sources.insert(old);
            }
        }

        let tx = crate::new_tx_id();
        let mut plan = Plan {
            tx: tx.clone(),
            unit: unit.to_string(),
            unit_kind: kind,
            ops: Vec::new(),
            conflicts: Vec::new(),
            lock: None,
            actions: Vec::new(),
        };
        let mut disk_cache: HashMap<String, String> = HashMap::new();
        let mut group_locals: Vec<Entry> = Vec::new();
        let mut group_remotes: Vec<Entry> = Vec::new();
        let mut case_keys: HashMap<String, String> = HashMap::new();
        if self.case_insensitive {
            for s in self.store().live_entries(unit)? {
                case_keys.insert(rpath::case_key(&s.entry.path), s.entry.path.clone());
            }
        }
        let conflict_dir = self.conflict_root().join(unit).join(&tx);

        for (path, class, d) in decisions {
            let l = local.get(&path);
            let r = remote_entries.get(&path);
            let action = |kind: ActionKind, reason: String| PlannedAction { path: path.clone(), kind, reason };

            if nested.iter().any(|n| rpath::is_within(&path, n)) {
                plan.actions.push(action(ActionKind::Skip, "belongs to a nested unit".into()));
                continue;
            }
            if respell_sources.contains(&path) {
                // Old spelling of a renamed entry: only its row becomes a tombstone.
                if let (Decision::TakeRemote(vv), Some(l)) = (&d, l) {
                    let tomb = Entry::tombstone(&path, l.entry.kind, vv.clone());
                    plan.ops.push(db_op(&path, &l.disk, tomb, "renamed (spelling only)"));
                }
                continue;
            }
            if let Some(from) = respelled.get(&path) {
                let s = l.expect("respelled entry is local");
                let mut e = s.entry.clone();
                if let Decision::AdoptVv(vv) | Decision::KeepLocalWins(vv) = &d {
                    e.vv = vv.clone();
                }
                plan.ops.push(Op {
                    path: path.clone(),
                    disk: s.disk.clone(),
                    kind: OpKind::Rename,
                    phase: Phase::Dirs,
                    rank: rank_of(class),
                    entry: e,
                    staged: None,
                    source: Some(Source::Rename { from: from.clone() }),
                    reason: "renamed (spelling only)".into(),
                });
                let label = format!("renamed from {}", rpath::file_name(from));
                plan.actions.push(action(ActionKind::Rename, label));
            }
            let in_group = git_state_conflict && class.is_some_and(|c| c.is_git_state());
            if in_group && matches!(d, Decision::TakeRemote(_) | Decision::MergeReflog(_) | Decision::Conflict(_)) {
                if let Some(r) = r {
                    if let Some(l) = l {
                        group_locals.push(l.entry.clone());
                    }
                    group_remotes.push(r.clone());
                }
                plan.actions.push(action(ActionKind::Conflict, "held back: Git state conflict".into()));
                continue;
            }

            match d {
                Decision::Nothing => {}
                Decision::KeepLocal => {
                    plan.actions.push(action(ActionKind::KeepLocal, "local version is newer".into()));
                }
                Decision::AdoptVv(vv) | Decision::KeepLocalWins(vv) => {
                    let l = l.expect("adopt/keep requires local");
                    let mut s = l.clone();
                    s.entry.vv = vv.clone();
                    plan.ops.push(db_op(&path, &s.disk, s.entry, "version vectors merged"));
                    plan.actions.push(action(ActionKind::Vv, format!("vv → {}", vv.display())));
                }
                Decision::Conflict(c) => {
                    let r = r.expect("conflict requires remote");
                    let locals: Vec<Entry> = l.map(|s| vec![s.entry.clone()]).unwrap_or_default();
                    if self.store().has_open_conflict(unit, &path, std::slice::from_ref(r))? {
                        plan.actions.push(action(ActionKind::Conflict, format!("{} (already recorded)", c.as_str())));
                        continue;
                    }
                    plan.conflicts.push(ConflictPlan {
                        path: path.clone(),
                        class: c.as_str().into(),
                        locals,
                        remotes: vec![r.clone()],
                        dir: conflict_dir.join(format!("{}", plan.conflicts.len())).to_string_lossy().into_owned(),
                    });
                    plan.actions.push(action(ActionKind::Conflict, c.as_str().into()));
                }
                Decision::MergeReflog(vv) => {
                    let (l, r) = (l.expect("merge requires local"), r.expect("merge requires remote"));
                    let (Some(lb), Some(rb)) = (l.entry.blob, r.blob) else {
                        continue;
                    };
                    let mut e = r.clone();
                    e.vv = vv;
                    let disk = l.disk.clone();
                    plan.ops.push(Op {
                        path: path.clone(),
                        disk,
                        kind: OpKind::Write,
                        phase: Phase::Writes,
                        rank: rank_of(class),
                        entry: e,
                        staged: Some(format!("{}", plan.ops.len())),
                        source: Some(Source::Reflog {
                            local_disk: l.disk.clone(),
                            local_blob: lb,
                            remote: rb,
                            remote_size: r.size,
                        }),
                        reason: "concurrent reflogs merged".into(),
                    });
                    plan.actions.push(action(ActionKind::Merge, "concurrent reflogs merged line-wise".into()));
                }
                Decision::TakeRemote(vv) => {
                    let synthetic;
                    let r = match r {
                        Some(r) => r,
                        None => {
                            // Mirror mode: remote does not know this local entry.
                            let l = l.expect("mirror delete requires local");
                            synthetic = Entry::tombstone(&path, l.entry.kind, l.entry.vv.clone());
                            &synthetic
                        }
                    };
                    let mut e = r.clone();
                    e.vv = vv.clone();
                    let local_live = l.filter(|s| !s.entry.deleted);
                    let reason = match (l, local.get(&path).map(|s| s.entry.vv.compare(&r.vv))) {
                        (None, _) => "new on peer".to_string(),
                        (Some(_), Some(macsync_core::VvOrdering::Dominated)) => {
                            format!(
                                "peer newer ({} → {})",
                                l.map(|s| s.entry.vv.display()).unwrap_or_default(),
                                r.vv.display()
                            )
                        }
                        _ if policy == Policy::MirrorReceive => "mirror: peer is authoritative".into(),
                        _ => "concurrent change resolved by rule".into(),
                    };
                    if e.deleted {
                        match local_live {
                            Some(s) => {
                                let is_obj = class.is_some_and(|c| c.is_object());
                                if is_obj && git_state_conflict {
                                    plan.actions.push(action(ActionKind::Skip, "object deletion held back".into()));
                                    continue;
                                }
                                plan.ops.push(Op {
                                    path: path.clone(),
                                    disk: s.disk.clone(),
                                    kind: OpKind::Delete,
                                    phase: if is_obj { Phase::ObjectDeletes } else { Phase::Deletes },
                                    rank: rank_of(class),
                                    entry: Entry::tombstone(&path, s.entry.kind, vv),
                                    staged: None,
                                    source: None,
                                    reason: reason.clone(),
                                });
                                plan.actions.push(action(ActionKind::Delete, reason));
                            }
                            None => {
                                let disk = l.map(|s| s.disk.clone()).unwrap_or_else(|| path.clone());
                                plan.ops.push(db_op(&path, &disk, Entry::tombstone(&path, e.kind, vv), "tombstone"));
                            }
                        }
                        continue;
                    }
                    // Case collision on case-insensitive volumes.
                    if self.case_insensitive && local_live.is_none() {
                        let k = rpath::case_key(&path);
                        if let Some(other) = case_keys.get(&k).filter(|o| **o != path) {
                            plan.conflicts.push(ConflictPlan {
                                path: path.clone(),
                                class: ConflictClass::CaseCollision.as_str().into(),
                                locals: local.get(other).map(|s| vec![s.entry.clone()]).unwrap_or_default(),
                                remotes: vec![r.clone()],
                                dir: conflict_dir
                                    .join(format!("{}", plan.conflicts.len()))
                                    .to_string_lossy()
                                    .into_owned(),
                            });
                            plan.actions.push(action(ActionKind::Conflict, format!("case collision with {other}")));
                            continue;
                        }
                        case_keys.insert(k, path.clone());
                    }
                    // Git never runs its `*.sample` files: no warning for those.
                    if class == Some(GitClass::Hook) && !path.ends_with(".sample") {
                        let _ = self.store().log(
                            unit,
                            "warning",
                            &path,
                            "hook changed by sync (never executed by MacSync)",
                            "",
                        );
                    } else if path == ".git/config" && local_live.is_some() {
                        let _ = self.store().log(
                            unit,
                            "warning",
                            &path,
                            "git config changed by sync (MacSync ignores its programs)",
                            "",
                        );
                    }
                    let disk = self.disk_for(unit, &path, &local, &mut disk_cache)?;
                    let same_kind = local_live.is_some_and(|s| s.entry.kind == e.kind);
                    let (kind_op, phase, akind) = match e.kind {
                        Kind::Dir if same_kind => (OpKind::Meta, Phase::Writes, ActionKind::Meta),
                        Kind::Dir => (OpKind::Mkdir, Phase::Dirs, ActionKind::Write),
                        Kind::Symlink => (OpKind::Symlink, Phase::Writes, ActionKind::Write),
                        Kind::File if local_live.is_some_and(|s| s.entry.same_content(&e)) => {
                            (OpKind::Meta, Phase::Writes, ActionKind::Meta)
                        }
                        Kind::File => (OpKind::Write, Phase::Writes, ActionKind::Write),
                    };
                    let phase =
                        if class == Some(GitClass::Index) && kind_op == OpKind::Write { Phase::Index } else { phase };
                    let source = (kind_op == OpKind::Write)
                        .then(|| Source::Blob { hash: e.blob.unwrap_or_default(), size: e.size });
                    plan.ops.push(Op {
                        path: path.clone(),
                        disk,
                        kind: kind_op,
                        phase,
                        rank: rank_of(class),
                        staged: (kind_op == OpKind::Write).then(|| format!("{}", plan.ops.len())),
                        entry: e,
                        source,
                        reason: reason.clone(),
                    });
                    plan.actions.push(action(akind, reason));
                }
            }
        }
        if !group_remotes.is_empty() {
            let path = ".git".to_string();
            if self.store().has_open_conflict(unit, &path, &group_remotes)? {
                plan.actions.push(PlannedAction {
                    path,
                    kind: ActionKind::Conflict,
                    reason: "git-state (already recorded)".into(),
                });
            } else {
                plan.conflicts.push(ConflictPlan {
                    path,
                    class: ConflictClass::GitState.as_str().into(),
                    locals: group_locals,
                    remotes: group_remotes,
                    dir: conflict_dir.join("git-state").to_string_lossy().into_owned(),
                });
            }
        }
        plan.sort();
        rep.lap("plan", &mut t);
        rep.actions = plan.actions.clone();
        rep.writes = plan
            .ops
            .iter()
            .filter(|o| {
                matches!(o.kind, OpKind::Write | OpKind::Mkdir | OpKind::Symlink | OpKind::Meta | OpKind::Rename)
            })
            .count();
        rep.deletes = plan.ops.iter().filter(|o| o.kind == OpKind::Delete).count();
        rep.conflicts = plan.conflicts.len()
            + plan
                .actions
                .iter()
                .filter(|a| a.kind == ActionKind::Conflict && a.reason.ends_with("(already recorded)"))
                .count();

        if opts.dry_run {
            rep.outcome = Outcome::DryRun;
            return Ok(rep);
        }

        // 4. Staging: fetch or clone every needed blob and verify its hash.
        let staging = self.staging_root().join(&tx);
        std::fs::create_dir_all(&staging)?;
        let result = self.stage_and_apply(peer, &mut plan, &local, &staging, &mut rep);
        // An interrupted apply keeps its staging directory for recovery.
        if self.store().tx_pending()?.iter().all(|t| t.id != tx) {
            let _ = ops::remove_any(&staging);
        }
        result?;

        // 5. Post-apply: refresh index stat cache, re-capture, verify.
        let mut t = std::time::Instant::now();
        self.post_apply(&plan, &mut rep)?;
        rep.lap("post-apply", &mut t);
        rep.outcome = Outcome::Applied;
        Ok(rep)
    }

    fn stage_and_apply(
        &self,
        peer: &dyn PeerApi,
        plan: &mut Plan,
        local: &HashMap<String, Stored>,
        staging: &Path,
        rep: &mut PullReport,
    ) -> Result<()> {
        let mut needs: BTreeMap<Hash, Need> = BTreeMap::new();
        let mut add_need = |hash: Hash, size: u64, dest: PathBuf, label: &str| {
            needs.entry(hash).or_insert(Need { size, dests: Vec::new(), label: label.to_string() }).dests.push(dest);
        };
        let mut reflog_remote: Vec<(usize, PathBuf)> = Vec::new();
        for (i, op) in plan.ops.iter().enumerate() {
            match &op.source {
                Some(Source::Blob { hash, size }) => {
                    add_need(*hash, *size, staging.join(op.staged.as_ref().expect("staged name")), &op.path);
                }
                Some(Source::Reflog { remote, remote_size, .. }) => {
                    let p = staging.join(format!("reflog-remote-{i}"));
                    add_need(*remote, *remote_size, p.clone(), &op.path);
                    reflog_remote.push((i, p));
                }
                Some(Source::File { .. }) | Some(Source::Rename { .. }) | None => {}
            }
        }
        for c in &plan.conflicts {
            let dir = PathBuf::from(&c.dir);
            for r in &c.remotes {
                if r.deleted || r.kind != Kind::File {
                    continue;
                }
                if let Some(b) = r.blob {
                    add_need(b, r.size, dir.join(if r.path.is_empty() { "_unit_root" } else { &r.path }), &r.path);
                }
            }
        }

        let total: u64 = needs.values().map(|n| n.size).sum();
        if total > 0 {
            let free = ops::available_space(staging).unwrap_or(u64::MAX);
            if free < total.saturating_add(64 * 1024 * 1024) {
                anyhow::bail!("not enough free space: need {total} bytes, {free} available");
            }
        }
        let mut t = std::time::Instant::now();
        self.fetch(peer, &needs, rep, staging)?;
        rep.lap("fetch", &mut t);

        // Materialise conflict-store symlinks/dirs.
        for c in &plan.conflicts {
            let dir = PathBuf::from(&c.dir);
            std::fs::create_dir_all(&dir)?;
            for r in &c.remotes {
                let p = dir.join(if r.path.is_empty() { "_unit_root" } else { &r.path });
                match r.kind {
                    Kind::Symlink if !r.deleted => {
                        if let Some(parent) = p.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        let t = <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(
                            r.target.as_deref().unwrap_or_default(),
                        );
                        let _ = std::os::unix::fs::symlink(Path::new(t), &p);
                    }
                    Kind::Dir if !r.deleted => std::fs::create_dir_all(&p)?,
                    _ => {}
                }
            }
        }

        // Reflog merges.
        for (i, remote_path) in reflog_remote {
            let op = &mut plan.ops[i];
            let Some(Source::Reflog { local_disk, local_blob, .. }) = op.source.clone() else {
                continue;
            };
            let local_abs = macsync_fs::abs_path(&self.cfg.root, &plan.unit, &local_disk);
            let Some(local_bytes) = ops::read_verified(&local_abs, &local_blob)? else {
                return retry(format!("{} changed locally", op.path));
            };
            let merged = macsync_gitaware::reflog::merge(&local_bytes, &std::fs::read(&remote_path)?);
            let h = Hash::of(&merged);
            ops::write_new(&staging.join(op.staged.as_ref().expect("staged")), &merged)?;
            op.entry.blob = Some(h);
            op.entry.cmp = Some(h);
            op.entry.size = merged.len() as u64;
        }

        // Pre-apply check: never overwrite a local change that was not captured.
        if plan.unit_kind == UnitKind::Git {
            let g = self.gate.check(&macsync_fs::unit_dir(&self.cfg.root, &plan.unit));
            if !g.is_open() {
                return retry(g.describe());
            }
        }
        for op in &plan.ops {
            if op.kind == OpKind::DbOnly {
                continue;
            }
            let abs = macsync_fs::abs_path(&self.cfg.root, &plan.unit, &op.disk);
            let on_disk = scan::stat(&abs, op.path.clone(), op.disk.clone()).ok().flatten();
            match (local.get(&op.path).filter(|s| !s.entry.deleted), on_disk) {
                (Some(s), Some(d)) => {
                    let unchanged = s.entry.kind == d.kind
                        && (d.kind == Kind::Dir
                            || (s.entry.size == d.size
                                && s.entry.mtime_ns == d.mtime_ns
                                && s.stat.ino == d.ino
                                && s.stat.ctime_ns == d.ctime_ns));
                    if !unchanged {
                        return retry(format!("{} changed locally since last capture", op.path));
                    }
                }
                (Some(_), None) => return retry(format!("{} was removed locally", op.path)),
                (None, Some(d)) => {
                    if !(op.kind == OpKind::Mkdir && d.kind == Kind::Dir) {
                        return retry(format!("{} was created locally", op.path));
                    }
                }
                (None, None) => {}
            }
        }

        rep.lap("pre-check", &mut t);
        // Execute.
        let r = self.execute(plan, staging);
        rep.lap("apply", &mut t);
        r
    }

    fn fetch(
        &self,
        peer: &dyn PeerApi,
        needs: &BTreeMap<Hash, Need>,
        rep: &mut PullReport,
        staging: &Path,
    ) -> Result<()> {
        let mut remote_small: Vec<Hash> = Vec::new();
        let mut remote_large: Vec<Hash> = Vec::new();
        let _guard = TransferGuard(self);
        self.update_transfer(|t| {
            *t = Some(crate::Transfer {
                unit: rep.unit.clone(),
                files_total: needs.len() as u64,
                bytes_total: needs.values().map(|n| n.size).sum(),
                started_ms: macsync_fs::time::now_ms(),
                ..Default::default()
            })
        });
        let progress = |files: u64, bytes: u64, current: &str| {
            self.update_transfer(|t| {
                if let Some(t) = t {
                    t.files_done += files;
                    t.bytes_done += bytes;
                    if !current.is_empty() {
                        t.current = current.to_string();
                    }
                }
            })
        };
        for (hash, need) in needs {
            for d in &need.dests {
                if let Some(p) = d.parent().filter(|p| *p != staging) {
                    std::fs::create_dir_all(p)?;
                }
            }
            if self.dedup_local(hash, &need.dests[0])? {
                rep.bytes_deduped += need.size;
                copy_rest(&need.dests)?;
                progress(1, need.size, &need.label);
            } else if need.size <= limits::SMALL_FILE {
                remote_small.push(*hash);
            } else {
                remote_large.push(*hash);
            }
        }
        // More data, more connections (up to MAX_LANES in all).
        let remote_bytes: u64 = remote_small.iter().chain(&remote_large).map(|h| needs[h].size).sum();
        let lanes = peer.lanes(lane_count(remote_bytes));
        if !lanes.is_empty() {
            tracing::debug!(unit = %rep.unit, extra = lanes.len(), bytes = remote_bytes, "parallel transfer");
        }
        let fetched = AtomicU64::new(0);
        // Bundles of small files, written by a thread pool.
        let mut bundles: Vec<&[Hash]> = Vec::new();
        let mut i = 0;
        while i < remote_small.len() {
            let mut bytes = 0u64;
            let mut j = i;
            while j < remote_small.len() && j - i < limits::BUNDLE_COUNT && bytes < limits::BUNDLE_BYTES {
                bytes += needs[&remote_small[j]].size;
                j += 1;
            }
            bundles.push(&remote_small[i..j]);
            i = j;
        }
        run_lanes(peer, &lanes, &bundles, &|p: &dyn PeerApi, batch: &&[Hash]| {
            progress(0, 0, &needs[&batch[0]].label);
            let mut blobs = p.blobs(batch)?;
            blobs.resize(batch.len(), None);
            let mut items: Vec<(&Need, Vec<u8>)> = Vec::with_capacity(batch.len());
            for (h, b) in batch.iter().zip(blobs) {
                let data = match b {
                    Some(d) if Hash::of(&d) == *h => d,
                    _ => self.refetch_small(p, h)?,
                };
                items.push((&needs[h], data));
            }
            write_parallel(&items)?;
            fetched.fetch_add(items.iter().map(|(_, d)| d.len() as u64).sum(), Ordering::Relaxed);
            progress(batch.len() as u64, items.iter().map(|(n, _)| n.size).sum(), "");
            Ok(())
        })?;
        // Large files in ranges, the ranges of one file over all connections.
        for h in remote_large {
            let need = &needs[&h];
            progress(0, 0, &need.label);
            let mut attempt = 0;
            loop {
                attempt += 1;
                match self.fetch_large(peer, &lanes, &h, need, &fetched)? {
                    true => break,
                    false if attempt < 3 => continue,
                    false => anyhow::bail!("hash mismatch for blob {} after 3 attempts", h.short()),
                }
            }
            copy_rest(&need.dests)?;
            progress(1, 0, "");
        }
        rep.bytes_fetched += fetched.into_inner();
        // One bundled flush for everything staged (instead of one per file).
        let all: Vec<PathBuf> = needs.values().flat_map(|n| n.dests.iter().cloned()).collect();
        ops::sync_paths(&all)?;
        Ok(())
    }

    fn refetch_small(&self, peer: &dyn PeerApi, h: &Hash) -> Result<Vec<u8>> {
        for _ in 0..2 {
            match peer.blobs(std::slice::from_ref(h))?.into_iter().next().flatten() {
                Some(d) if Hash::of(&d) == *h => return Ok(d),
                Some(_) => continue,
                None => return retry(format!("blob {} no longer available on peer", h.short())),
            }
        }
        anyhow::bail!("hash mismatch for blob {} after 3 attempts", h.short())
    }

    /// One large file: its ranges are fetched over `peer` and `lanes` at
    /// once and written in place; true if the result has the right hash.
    fn fetch_large(
        &self,
        peer: &dyn PeerApi,
        lanes: &[Box<dyn PeerApi + Send + Sync>],
        h: &Hash,
        need: &Need,
        fetched: &AtomicU64,
    ) -> Result<bool> {
        use std::os::unix::fs::FileExt;
        let dest = &need.dests[0];
        let f = std::fs::File::create(dest)?;
        f.set_len(need.size)?;
        let ranges: Vec<(u64, u64)> = (0..need.size)
            .step_by(limits::RANGE_CHUNK as usize)
            .map(|off| (off, (need.size - off).min(limits::RANGE_CHUNK)))
            .collect();
        run_lanes(peer, lanes, &ranges, &|p: &dyn PeerApi, &(start, len): &(u64, u64)| {
            let mut off = start;
            while off < start + len {
                let Some(chunk) = p.blob_range(h, off, start + len - off)? else {
                    return retry(format!("blob {} changed on peer during transfer", h.short()));
                };
                if chunk.is_empty() {
                    return retry(format!("blob {} shorter than expected", h.short()));
                }
                f.write_all_at(&chunk, off)?;
                off += chunk.len() as u64;
                fetched.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                self.update_transfer(|t| {
                    if let Some(t) = t {
                        t.bytes_done += chunk.len() as u64;
                    }
                });
            }
            Ok(())
        })?;
        drop(f);
        Ok(ops::hash_file(dest)? == *h)
    }

    /// Try to materialise a blob from a local copy (copy-on-write clone where
    /// supported). The copy is verified before use.
    fn dedup_local(&self, hash: &Hash, dest: &Path) -> Result<bool> {
        let candidates = self.store().find_blob(hash)?;
        for (unit, _path, disk) in candidates {
            let src = macsync_fs::abs_path(&self.cfg.root, &unit, &disk);
            if ops::clone_or_copy(&src, dest).is_ok() && ops::hash_file(dest).ok() == Some(*hash) {
                return Ok(true);
            }
            let _ = std::fs::remove_file(dest);
        }
        Ok(false)
    }

    /// Local disk path for a canonical path: the existing spelling if known,
    /// otherwise the parent's spelling plus the canonical name.
    fn disk_for(
        &self,
        unit: &str,
        path: &str,
        local: &HashMap<String, Stored>,
        cache: &mut HashMap<String, String>,
    ) -> Result<String> {
        if path.is_empty() {
            return Ok(String::new());
        }
        if let Some(d) = cache.get(path) {
            return Ok(d.clone());
        }
        let known = match local.get(path) {
            Some(s) => Some(s.disk.clone()),
            None => self.store().entry(unit, path)?.map(|s| s.disk),
        };
        let d = match known {
            Some(d) => d,
            None => {
                let parent = rpath::parent(path).unwrap_or("");
                let pd = self.disk_for(unit, parent, local, cache)?;
                rpath::join(&pd, rpath::file_name(path))
            }
        };
        cache.insert(path.to_string(), d.clone());
        Ok(d)
    }

    fn post_apply(&self, plan: &Plan, rep: &mut PullReport) -> Result<()> {
        let unit = plan.unit.as_str();
        let unit_dir = macsync_fs::unit_dir(&self.cfg.root, unit);
        let is_git = plan.unit_kind == UnitKind::Git && macsync_gitaware::resolve_git_dir(&unit_dir).is_some();
        let touched_tree = plan.ops.iter().any(|o| o.kind != OpKind::DbOnly && !o.path.starts_with(".git/"));
        let touched_index = plan.ops.iter().any(|o| o.phase == Phase::Index);
        let touched_objects = plan.ops.iter().any(|o| {
            o.kind != OpKind::DbOnly
                && (o.path.starts_with(".git/objects")
                    || o.path.starts_with(".git/refs")
                    || o.path == ".git/packed-refs"
                    || o.path == ".git/HEAD")
        });
        let git = is_git && macsync_gitaware::cmd::available();
        if git && (touched_tree || touched_index) {
            if let Err(e) = macsync_gitaware::cmd::refresh_index(&unit_dir) {
                let _ = self.store().log(unit, "warning", ".git/index", "index refresh failed", &e.to_string());
            }
        }
        if git && touched_objects && self.cfg.fsck_after_apply {
            if let Err(msg) = macsync_gitaware::cmd::fsck_connectivity(&unit_dir)? {
                let restored = self.restore_object_deletions(plan)?;
                let _ = self.store().log(
                    unit,
                    "error",
                    ".git",
                    "git fsck --connectivity-only failed after apply",
                    &format!("{msg}; restored {restored} objects from trash"),
                );
                tracing::error!(unit, %msg, restored, "fsck failed after apply");
            }
        }
        let scan = self.scan_unit_locked(unit, &ScanOptions::default())?;
        rep.local_root = scan.root;
        rep.converged = scan.root == rep.remote_root;
        let open = self.store().open_conflict_count(unit)?;
        let status = if open > 0 {
            "conflict"
        } else if rep.converged {
            "in sync"
        } else {
            "partially synced"
        };
        self.store().set_unit_status(unit, status, rep.converged)?;
        Ok(())
    }

    /// Identical roots prove that open conflicts of the unit are moot. Adopt
    /// the peer's version vectors for those paths (otherwise the next local
    /// edit would look concurrent again) and close the conflicts.
    fn settle_conflicts(&self, peer: &dyn PeerApi, unit: &str) -> Result<()> {
        let open: Vec<_> = self.store().conflicts(false)?.into_iter().filter(|c| c.unit == unit).collect();
        if open.is_empty() {
            return Ok(());
        }
        let paths: Vec<String> = open.iter().flat_map(|c| c.remotes.iter().map(|r| r.path.clone())).collect();
        let remote: HashMap<String, Entry> =
            peer.entries(unit, &paths, &[])?.into_iter().map(|e| (e.path.clone(), e)).collect();
        let store = self.store();
        store.atomically(|st| {
            for p in &paths {
                if let (Some(mut l), Some(r)) = (st.entry(unit, p)?, remote.get(p)) {
                    if l.entry.same_state(r) {
                        l.entry.vv = l.entry.vv.merge(&r.vv);
                        st.put_entry(unit, &l)?;
                    }
                }
            }
            for c in &open {
                st.resolve_conflict(c.id, "superseded")?;
                st.log(unit, "resolve", &c.path, "conflict moot: both sides identical", "")?;
            }
            Ok(())
        })
    }

    fn after_converged(&self, unit: &str) -> Result<()> {
        let open = self.store().open_conflict_count(unit)?;
        self.store().set_unit_status(unit, if open > 0 { "conflict" } else { "in sync" }, true)
    }
}

fn db_op(path: &str, disk: &str, entry: Entry, reason: &str) -> Op {
    Op {
        path: path.to_string(),
        disk: disk.to_string(),
        kind: OpKind::DbOnly,
        phase: Phase::Db,
        rank: 0,
        entry,
        staged: None,
        source: None,
        reason: reason.to_string(),
    }
}

/// Paralleles Schreiben (concept): staging files are written by a small
/// thread pool; durability follows with one bundled flush.
/// All connections of one transfer, including the main one.
const MAX_LANES: usize = 4;
/// Outstanding bytes per connection before another one is opened.
const LANE_BYTES: u64 = 32 * 1024 * 1024;

/// Extra connections for a transfer of `bytes`.
fn lane_count(bytes: u64) -> usize {
    ((bytes / LANE_BYTES) as usize).min(MAX_LANES - 1)
}

/// Run `work` for every job, spread over `peer` (this thread) and one thread
/// per lane. A failing lane gives its job back and stops; errors on `peer`
/// abort the transfer.
fn run_lanes<J: Sync>(
    peer: &dyn PeerApi,
    lanes: &[Box<dyn PeerApi + Send + Sync>],
    jobs: &[J],
    work: &(dyn Fn(&dyn PeerApi, &J) -> Result<()> + Sync),
) -> Result<()> {
    if lanes.is_empty() || jobs.len() < 2 {
        return jobs.iter().try_for_each(|j| work(peer, j));
    }
    let next = AtomicUsize::new(0);
    let back: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    let stop = AtomicBool::new(false);
    let take = || -> Option<usize> {
        if stop.load(Ordering::SeqCst) {
            return None;
        }
        if let Some(i) = back.lock().unwrap_or_else(|e| e.into_inner()).pop() {
            return Some(i);
        }
        let i = next.fetch_add(1, Ordering::SeqCst);
        (i < jobs.len()).then_some(i)
    };
    std::thread::scope(|s| {
        for lane in lanes {
            let (take, back) = (&take, &back);
            s.spawn(move || {
                while let Some(i) = take() {
                    if let Err(e) = work(lane.as_ref(), &jobs[i]) {
                        tracing::debug!(error = %format!("{e:#}"), "transfer connection dropped");
                        back.lock().unwrap_or_else(|e| e.into_inner()).push(i);
                        return;
                    }
                }
            });
        }
        while let Some(i) = take() {
            if let Err(e) = work(peer, &jobs[i]) {
                stop.store(true, Ordering::SeqCst);
                return Err(e);
            }
        }
        Ok(())
    })?;
    // Jobs a lane gave back after this thread ran out of work.
    let rest = std::mem::take(&mut *back.lock().unwrap_or_else(|e| e.into_inner()));
    rest.into_iter().try_for_each(|i| work(peer, &jobs[i]))
}

fn write_parallel(items: &[(&Need, Vec<u8>)]) -> Result<()> {
    const THREADS: usize = 8;
    if items.len() < 64 {
        for (need, data) in items {
            ops::write_unsynced(&need.dests[0], data)?;
            copy_rest(&need.dests)?;
        }
        return Ok(());
    }
    let chunk = items.len().div_ceil(THREADS);
    std::thread::scope(|s| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|c| {
                s.spawn(move || -> Result<()> {
                    for (need, data) in c {
                        ops::write_unsynced(&need.dests[0], data)?;
                        copy_rest(&need.dests)?;
                    }
                    Ok(())
                })
            })
            .collect();
        for h in handles {
            h.join().map_err(|_| anyhow::anyhow!("staging writer panicked"))??;
        }
        Ok(())
    })
}

fn copy_rest(dests: &[PathBuf]) -> Result<()> {
    for d in &dests[1..] {
        ops::clone_or_copy(&dests[0], d)?;
    }
    Ok(())
}

/// Merkle descent: returns the exact paths whose entries must be compared.
///
/// A directory that exists on one side only is followed down its tree
/// (remote) or listed from the local tree, never fetched as a whole subtree:
/// below a build directory can lie hundreds of thousands of tombstones that
/// only matter where the other side still has a file.
fn descend(peer: &dyn PeerApi, unit: &str, local: &Tree, remote_root: Hash) -> Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    if local.root() == remote_root {
        return Ok(paths);
    }
    paths.insert(String::new());
    let mut queue: Vec<String> = vec![String::new()];
    while !queue.is_empty() {
        let take = queue.len().min(limits::TREE_BATCH);
        let batch: Vec<String> = queue.drain(..take).collect();
        let listings = peer.tree(unit, &batch)?;
        for l in listings {
            if !batch.contains(&l.dir) {
                continue;
            }
            let Some(rc) = l.children else {
                // Not a directory there (any more): our files below it.
                local_subtree(local, &l.dir, &mut paths);
                continue;
            };
            let lc = local.children(&l.dir).unwrap_or(&[]);
            let mut names: BTreeMap<
                &str,
                (Option<&macsync_core::merkle::Child>, Option<&macsync_core::merkle::Child>),
            > = BTreeMap::new();
            for c in &rc {
                names.entry(c.name.as_str()).or_default().1 = Some(c);
            }
            for c in lc {
                names.entry(c.name.as_str()).or_default().0 = Some(c);
            }
            for (name, (lc, rc)) in names {
                let path = rpath::join(&l.dir, name);
                if rpath::validate(&path).is_err() {
                    continue;
                }
                match (lc, rc) {
                    (Some(a), Some(b)) if a.kind == b.kind && a.hash == b.hash => {}
                    (Some(a), Some(b)) if a.kind == Kind::Dir && b.kind == Kind::Dir => {
                        paths.insert(path.clone());
                        queue.push(path);
                    }
                    (a, b) => {
                        paths.insert(path.clone());
                        if a.is_some_and(|c| c.kind == Kind::Dir) {
                            local_subtree(local, &path, &mut paths);
                        }
                        if b.is_some_and(|c| c.kind == Kind::Dir) {
                            queue.push(path);
                        }
                    }
                }
            }
        }
    }
    Ok(paths)
}

/// All live paths below `dir` in the local tree.
fn local_subtree(local: &Tree, dir: &str, paths: &mut BTreeSet<String>) {
    let mut todo = vec![dir.to_string()];
    while let Some(d) = todo.pop() {
        for c in local.children(&d).unwrap_or(&[]) {
            let p = rpath::join(&d, &c.name);
            if c.kind == Kind::Dir {
                todo.push(p.clone());
            }
            paths.insert(p);
        }
    }
}

fn fetch_entries(peer: &dyn PeerApi, unit: &str, paths: &BTreeSet<String>) -> Result<HashMap<String, Entry>> {
    let mut out = HashMap::new();
    let wanted = paths;
    let paths: Vec<String> = paths.iter().cloned().collect();
    let mut accept = |e: Entry| {
        if rpath::validate(&e.path).is_ok() {
            out.insert(e.path.clone(), e);
        }
    };
    for chunk in paths.chunks(limits::ENTRY_BATCH) {
        // Only what was asked for: a peer must not slip in other paths.
        for e in peer.entries(unit, chunk, &[])? {
            if wanted.contains(&e.path) {
                accept(e);
            }
        }
    }
    Ok(out)
}

/// Remove live entries below a path that the same delivery declares a
/// symbolic link (`x → /elsewhere` plus `x/file`): writing them would follow
/// the link out of the sync root. Returns the removed paths.
fn drop_below_symlinks(entries: &mut HashMap<String, Entry>) -> Vec<String> {
    let links: Vec<String> =
        entries.values().filter(|e| e.kind == Kind::Symlink && !e.deleted).map(|e| e.path.clone()).collect();
    if links.is_empty() {
        return Vec::new();
    }
    let below = |p: &str| links.iter().any(|l| p != l && rpath::is_within(p, l));
    let mut dropped: Vec<String> =
        entries.values().filter(|e| !e.deleted && below(&e.path)).map(|e| e.path.clone()).collect();
    for p in &dropped {
        entries.remove(p);
    }
    dropped.sort();
    dropped
}

/// Pairs `(new, old)` where the peer renamed `old` to `new` and the two
/// differ only in case or Unicode normalisation: the peer deleted `old`
/// (which is live here) and created `new` (which is not live here).
/// Ambiguous matches and entries of nested units are left alone.
fn case_renames(
    decisions: &[(String, Option<GitClass>, Decision)],
    local: &HashMap<String, Stored>,
    remote: &HashMap<String, Entry>,
    nested: &[String],
) -> Vec<(String, String)> {
    let worktree = |c: &Option<GitClass>| c.is_none_or(|c| c == GitClass::Worktree);
    let outside_nested = |p: &str| !nested.iter().any(|n| rpath::is_within(p, n));
    let mut gone: HashMap<String, Vec<&str>> = HashMap::new();
    for (p, c, d) in decisions {
        let deleted_remotely = remote.get(p).is_some_and(|r| r.deleted);
        let live_here = local.get(p).is_some_and(|l| !l.entry.deleted);
        if matches!(d, Decision::TakeRemote(_)) && deleted_remotely && live_here && worktree(c) && outside_nested(p) {
            gone.entry(rpath::case_key(p)).or_default().push(p);
        }
    }
    let mut created: HashMap<String, Vec<&str>> = HashMap::new();
    for (p, c, d) in decisions {
        let live_remotely = remote.get(p).is_some_and(|r| !r.deleted);
        let live_here = local.get(p).is_some_and(|l| !l.entry.deleted);
        if matches!(d, Decision::TakeRemote(_)) && live_remotely && !live_here && worktree(c) && outside_nested(p) {
            created.entry(rpath::case_key(p)).or_default().push(p);
        }
    }
    let mut out = Vec::new();
    for (k, news) in &created {
        let Some(olds) = gone.get(k) else {
            continue;
        };
        if let ([new], [old]) = (news.as_slice(), olds.as_slice()) {
            if local[*old].entry.kind == remote[*new].kind {
                out.push((new.to_string(), old.to_string()));
            }
        }
    }
    out
}

/// Disk spelling of `new` given the disk spelling of `old`: components whose
/// name changed take the new spelling, the others keep their disk spelling.
fn respell_disk(old_disk: &str, old: &str, new: &str) -> String {
    let (d, o, n): (Vec<&str>, Vec<&str>, Vec<&str>) =
        (old_disk.split('/').collect(), old.split('/').collect(), new.split('/').collect());
    if d.len() != n.len() || o.len() != n.len() {
        return new.to_string();
    }
    (0..n.len()).map(|i| if o[i] == n[i] { d[i] } else { n[i] }).collect::<Vec<_>>().join("/")
}
