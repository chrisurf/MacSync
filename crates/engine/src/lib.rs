//! The synchronisation engine.
//!
//! * [`Engine::scan_unit`] captures the local state of a unit into the index
//!   (stat cache, hashing, version vector increments, tombstones).
//! * The `serve_*` functions answer a peer's requests from the index.
//! * [`Engine::pull_unit`] reconciles a unit with a peer: Merkle descent,
//!   decisions, staging with hash verification and the Git-ordered atomic
//!   apply, guarded by a transaction journal.
//! * [`Engine::recover`] completes or discards interrupted transactions.
//! * [`Engine::resolve`] resolves recorded conflicts.

mod apply;
pub mod browse;
mod capture;
mod case;
mod plan;
mod pull;
mod resolve;
mod serve;
pub mod telemetry;
mod win;

pub use capture::{ScanOptions, ScanReport};
pub use plan::{ActionKind, PlannedAction};
pub use pull::{Outcome, PullOptions, PullQueue, PullReport, RetryLater};
pub use resolve::{Choice, ConflictView, Side};
pub use win::WinReport;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use anyhow::Result;
use macsync_core::proto::{DirListing, UnitRoot};
use macsync_core::{DeviceId, Entry, Hash, Policy, Tree, UnitKind};
use macsync_fs::Excludes;
use macsync_gitaware::Gate;
use macsync_store::Store;

/// Operating mode of this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// Both devices are equal (default).
    #[default]
    TwoWay,
    /// This device only sends (backup source); it never applies remote state.
    MirrorSend,
    /// This device mirrors the peer strictly; local changes are overwritten
    /// (old versions go to the trash).
    MirrorReceive,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s {
            "two-way" | "twoway" => Mode::TwoWay,
            "mirror-send" | "send" => Mode::MirrorSend,
            "mirror-receive" | "receive" => Mode::MirrorReceive,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::TwoWay => "two-way",
            Mode::MirrorSend => "mirror-send",
            Mode::MirrorReceive => "mirror-receive",
        }
    }
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub device: DeviceId,
    pub mode: Mode,
    pub excludes: Excludes,
    /// Drop `com.apple.quarantine` when applying remote files.
    pub strip_quarantine: bool,
    /// Run `git fsck --connectivity-only` after applying objects or refs.
    pub fsck_after_apply: bool,
    pub tombstone_ttl_days: u64,
    pub trash_retention_days: u64,
    pub journal_retention_days: u64,
}

impl EngineConfig {
    pub fn new(root: impl Into<PathBuf>, state_dir: impl Into<PathBuf>, device: impl Into<String>) -> EngineConfig {
        EngineConfig {
            root: root.into(),
            state_dir: state_dir.into(),
            device: device.into(),
            mode: Mode::TwoWay,
            excludes: Excludes::default(),
            strip_quarantine: false,
            fsck_after_apply: true,
            tombstone_ttl_days: 30,
            trash_retention_days: 14,
            journal_retention_days: 7,
        }
    }
}

/// The remote side as seen by the puller. Implemented in-process by
/// [`Engine`] itself (tests) and over the network by `macsync-net`.
pub trait PeerApi {
    fn roots(&self) -> Result<Vec<UnitRoot>>;
    fn tree(&self, unit: &str, dirs: &[String]) -> Result<Vec<DirListing>>;
    fn entries(&self, unit: &str, paths: &[String], prefixes: &[String]) -> Result<Vec<Entry>>;
    fn blobs(&self, hashes: &[Hash]) -> Result<Vec<Option<Vec<u8>>>>;
    fn blob_range(&self, hash: &Hash, offset: u64, len: u64) -> Result<Option<Vec<u8>>>;
    /// Up to `n` additional connections to the same peer for a large
    /// content transfer; dropped when the transfer ends. Default: none.
    fn lanes(&self, _n: usize) -> Vec<Box<dyn PeerApi + Send + Sync>> {
        Vec::new()
    }
}

/// Rows per store access when reading or writing a whole unit.
pub(crate) const STORE_PAGE: usize = 5_000;

/// Store access that takes longer than this is recorded (telemetry).
const SLOW_STORE: std::time::Duration = std::time::Duration::from_millis(250);

/// The locked store; reports a long hold when dropped (telemetry on).
pub struct StoreGuard<'a> {
    guard: MutexGuard<'a, Store>,
    since: Option<Instant>,
    site: &'static std::panic::Location<'static>,
}

impl std::ops::Deref for StoreGuard<'_> {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.guard
    }
}

impl std::ops::DerefMut for StoreGuard<'_> {
    fn deref_mut(&mut self) -> &mut Store {
        &mut self.guard
    }
}

impl Drop for StoreGuard<'_> {
    fn drop(&mut self) {
        if let Some(held) = self.since.map(|t| t.elapsed()).filter(|d| *d >= SLOW_STORE) {
            telemetry::event(
                "store-hold",
                serde_json::json!({"ms": telemetry::ms(held), "at": format!("{}:{}", self.site.file(), self.site.line())}),
            );
        }
    }
}

/// Error raised by the fault injection used in crash tests.
#[derive(Debug)]
pub struct InjectedCrash;

impl std::fmt::Display for InjectedCrash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("injected crash")
    }
}

impl std::error::Error for InjectedCrash {}

pub struct Engine {
    cfg: EngineConfig,
    store: Mutex<Store>,
    gate: Gate,
    unit_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    trees: Mutex<HashMap<String, Arc<Tree>>>,
    /// Pulls per unit deferred in a row because files changed on the peer
    /// during the transfer (see `leave_out_changed`).
    churn: Mutex<HashMap<String, u32>>,
    case_insensitive: bool,
    /// Fault injection: abort the apply after this many file operations
    /// (negative = disabled).
    fault_after_ops: AtomicI64,
    /// The transfer in progress (receiving side), for progress display.
    transfer: Mutex<Option<Transfer>>,
    /// Bytes served to the peer so far and when the last ones went out.
    served_bytes: AtomicU64,
    served_ms: AtomicI64,
}

/// Progress of the content transfer of one pull (receiving side).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Transfer {
    pub unit: String,
    pub files_total: u64,
    pub files_done: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub started_ms: i64,
    /// Unit-relative path of the file currently being transferred.
    pub current: String,
}

impl Engine {
    /// The content transfer in progress, if any.
    pub fn transfer(&self) -> Option<Transfer> {
        self.transfer.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn update_transfer(&self, f: impl FnOnce(&mut Option<Transfer>)) {
        f(&mut self.transfer.lock().unwrap_or_else(|e| e.into_inner()));
    }

    /// Total bytes served to the peer and the time of the last served bytes (ms).
    pub fn served(&self) -> (u64, i64) {
        (self.served_bytes.load(AtomicOrdering::Relaxed), self.served_ms.load(AtomicOrdering::Relaxed))
    }

    pub(crate) fn count_served(&self, bytes: u64) {
        if bytes > 0 {
            self.served_bytes.fetch_add(bytes, AtomicOrdering::Relaxed);
            self.served_ms.store(macsync_fs::time::now_ms(), AtomicOrdering::Relaxed);
        }
    }

    /// Open the engine: database, private directories, crash recovery.
    pub fn open(cfg: EngineConfig) -> Result<Engine> {
        std::fs::create_dir_all(&cfg.root)?;
        std::fs::create_dir_all(&cfg.state_dir)?;
        let private = cfg.root.join(macsync_fs::PRIVATE_DIR);
        // Staging and trash from the RepoSync era (≤ 0.2.x) move along.
        let legacy = cfg.root.join(macsync_fs::LEGACY_PRIVATE_DIR);
        if legacy.is_dir() && !private.exists() {
            std::fs::rename(&legacy, &private)?;
        }
        std::fs::create_dir_all(private.join("staging"))?;
        std::fs::create_dir_all(private.join("trash"))?;
        // Keep Spotlight away from staging and trash.
        let _ = std::fs::write(private.join(".metadata_never_index"), b"");
        let store = Store::open(&macsync_store::db_path(&cfg.state_dir)?)?;
        let case_insensitive = macsync_fs::ops::is_case_insensitive(&private).unwrap_or(false);
        let engine = Engine {
            cfg,
            store: Mutex::new(store),
            gate: Gate::new(),
            unit_locks: Mutex::new(HashMap::new()),
            trees: Mutex::new(HashMap::new()),
            churn: Mutex::new(HashMap::new()),
            case_insensitive,
            fault_after_ops: AtomicI64::new(-1),
            transfer: Mutex::new(None),
            served_bytes: AtomicU64::new(0),
            served_ms: AtomicI64::new(0),
        };
        engine.recover()?;
        Ok(engine)
    }

    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    pub fn device(&self) -> &str {
        &self.cfg.device
    }

    pub fn root(&self) -> &Path {
        &self.cfg.root
    }

    pub fn is_case_insensitive(&self) -> bool {
        self.case_insensitive
    }

    /// Access the store (short critical sections only). With telemetry on,
    /// a long wait or hold is recorded with the calling code location.
    #[track_caller]
    pub fn store(&self) -> StoreGuard<'_> {
        let site = std::panic::Location::caller();
        if !telemetry::enabled() {
            return StoreGuard { guard: self.store.lock().unwrap_or_else(|e| e.into_inner()), since: None, site };
        }
        let t = Instant::now();
        let guard = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let waited = t.elapsed();
        if waited >= SLOW_STORE {
            telemetry::event(
                "store-wait",
                serde_json::json!({"ms": telemetry::ms(waited), "at": format!("{}:{}", site.file(), site.line())}),
            );
        }
        StoreGuard { guard, since: Some(Instant::now()), site }
    }

    /// Fault injection for crash tests.
    pub fn inject_crash_after(&self, ops: i64) {
        self.fault_after_ops.store(ops, AtomicOrdering::SeqCst);
    }

    fn tick_fault(&self) -> Result<()> {
        let left = self.fault_after_ops.load(AtomicOrdering::SeqCst);
        if left < 0 {
            return Ok(());
        }
        if left == 0 {
            self.fault_after_ops.store(-1, AtomicOrdering::SeqCst);
            return Err(InjectedCrash.into());
        }
        self.fault_after_ops.store(left - 1, AtomicOrdering::SeqCst);
        Ok(())
    }

    fn unit_lock(&self, unit: &str) -> Arc<Mutex<()>> {
        let mut m = self.unit_locks.lock().unwrap_or_else(|e| e.into_inner());
        m.entry(unit.to_string()).or_default().clone()
    }

    fn policy_for(&self, unit: &str) -> Policy {
        match self.cfg.mode {
            Mode::MirrorReceive => Policy::MirrorReceive,
            _ => {
                let prefer = self.store().unit(unit).ok().flatten().map(|u| u.prefer).unwrap_or_default();
                Policy::TwoWay(prefer)
            }
        }
    }

    /// Merkle tree of a unit (cached).
    pub fn tree(&self, unit: &str) -> Result<Arc<Tree>> {
        if let Some(t) = self.trees.lock().unwrap_or_else(|e| e.into_inner()).get(unit) {
            return Ok(t.clone());
        }
        // Tombstones are not part of the tree.
        let entries = self.live_entries(unit)?;
        let t = Arc::new(Tree::build(entries.iter().map(|s| &s.entry)));
        self.trees.lock().unwrap_or_else(|e| e.into_inner()).insert(unit.to_string(), t.clone());
        Ok(t)
    }

    /// All live entries of `unit`, read in pages: the store is free for
    /// status and peer requests in between (a unit can hold 500 000 entries).
    pub(crate) fn live_entries(&self, unit: &str) -> Result<Vec<macsync_store::Stored>> {
        let mut out: Vec<macsync_store::Stored> = Vec::new();
        loop {
            let after = out.last().map(|s| s.entry.path.clone());
            let page = self.store().live_entries_after(unit, after.as_deref(), STORE_PAGE)?;
            let n = page.len();
            out.extend(page);
            if n < STORE_PAGE {
                return Ok(out);
            }
        }
    }

    fn invalidate_tree(&self, unit: &str) -> Result<Hash> {
        self.trees.lock().unwrap_or_else(|e| e.into_inner()).remove(unit);
        let t = self.tree(unit)?;
        self.store().set_unit_root(unit, t.root())?;
        Ok(t.root())
    }

    /// Split a path relative to the sync root into its unit (the longest
    /// known unit containing it, nested repositories included) and the
    /// directory below that unit. `None`: the root itself.
    pub fn split_path(&self, path: &str) -> Result<(Option<String>, String)> {
        let path = path.trim_matches('/');
        if path.is_empty() || path == "." {
            return Ok((None, String::new()));
        }
        let mut units: Vec<String> = self.store().units()?.into_iter().map(|u| u.name).collect();
        units.sort_by_key(|u| std::cmp::Reverse(u.len()));
        for u in units {
            if path == u {
                return Ok((Some(u), String::new()));
            }
            if let Some(rest) = path.strip_prefix(&format!("{u}/")) {
                return Ok((Some(u), rest.to_string()));
            }
        }
        // Not a unit here (yet): the first component is the unit.
        let (u, rest) = path.split_once('/').unwrap_or((path, ""));
        Ok((Some(u.to_string()), rest.to_string()))
    }

    /// Local roots of all known units.
    pub fn local_roots(&self) -> Result<Vec<UnitRoot>> {
        let units = self.store().units()?;
        let mut out = Vec::with_capacity(units.len());
        for u in units {
            let root = match u.root {
                Some(r) => r,
                None => self.invalidate_tree(&u.name)?,
            };
            out.push(UnitRoot { name: u.name, kind: u.kind, root });
        }
        Ok(out)
    }

    fn private_dir(&self) -> PathBuf {
        self.cfg.root.join(macsync_fs::PRIVATE_DIR)
    }

    fn staging_root(&self) -> PathBuf {
        self.private_dir().join("staging")
    }

    pub fn trash_root(&self) -> PathBuf {
        self.private_dir().join("trash")
    }

    pub fn conflict_root(&self) -> PathBuf {
        self.cfg.state_dir.join("conflicts")
    }

    /// Units below `unit` that are units of their own (nested repositories).
    fn nested_units_of(&self, unit: &str) -> Result<Vec<String>> {
        if unit == macsync_core::entry::ROOT_UNIT {
            return Ok(Vec::new());
        }
        let prefix = format!("{unit}/");
        Ok(self.store().units()?.into_iter().map(|u| u.name).filter(|n| n.starts_with(&prefix)).collect())
    }

    /// Remove expired trash, tombstones of in-sync units and old journal rows.
    pub fn housekeeping(&self, in_sync_units: &[String]) -> Result<()> {
        let now = macsync_fs::time::now_ms();
        let day = 86_400_000i64;
        let trash_cutoff = now - self.cfg.trash_retention_days as i64 * day;
        if let Ok(rd) = std::fs::read_dir(self.trash_root()) {
            for e in rd.flatten() {
                let old = e.metadata().and_then(|m| m.modified()).ok().is_some_and(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| (d.as_millis() as i64) < trash_cutoff)
                        .unwrap_or(false)
                });
                if old {
                    let _ = macsync_fs::ops::remove_any(&e.path());
                }
            }
        }
        let store = self.store();
        let tomb_cutoff = now - self.cfg.tombstone_ttl_days as i64 * day;
        for u in in_sync_units {
            store.purge_tombstones(u, tomb_cutoff)?;
        }
        store.prune_journal(now - self.cfg.journal_retention_days as i64 * day)?;
        Ok(())
    }

    /// Ensure a unit row exists with the given kind.
    fn ensure_unit(&self, unit: &str, kind: UnitKind) -> Result<()> {
        let store = self.store();
        match store.unit(unit)? {
            Some(u) if u.kind == kind => Ok(()),
            _ => store.upsert_unit(unit, kind),
        }
    }
}

/// Random transaction id (hex).
fn new_tx_id() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    format!("{}-{}", macsync_fs::time::compact_utc(macsync_fs::time::now_ms() / 1000), hex::encode(b))
}

/// Unit names are relative paths; they must never point into a `.git`
/// directory or MacSync's private directory.
pub fn validate_unit_name(name: &str) -> Result<()> {
    macsync_core::path::validate_unit(name).map_err(anyhow::Error::msg)?;
    if name.split('/').any(|c| c == ".git" || c == macsync_fs::PRIVATE_DIR) {
        anyhow::bail!("invalid unit name {name:?}");
    }
    Ok(())
}
