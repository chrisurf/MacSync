//! The running node: identity, engine, peer, and the daemon threads
//! (protocol server, watcher, scheduler, IPC).

use std::collections::{BTreeSet, HashMap};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use reposync_core::proto::Request;
use reposync_core::Hash;
use reposync_engine::{Engine, Outcome, PeerApi, PullOptions, PullReport, ScanOptions};
use reposync_net::client::HelloInfo;
use reposync_net::{discovery, Identity, NetPeer, Server};
use reposync_store::PeerRow;
use serde_json::{json, Value};

use crate::config::Config;

/// Identity + engine + configuration. Used by the daemon and, when no daemon
/// runs, directly by the CLI.
pub struct Node {
    pub cfg: Config,
    pub identity: Identity,
    pub engine: Arc<Engine>,
}

impl Node {
    pub fn open(cfg: Config) -> Result<Node> {
        let state = cfg.state_dir();
        let identity = Identity::load_or_create(&state)?;
        let engine = Engine::open(cfg.engine_config(&identity.device_id())?)?;
        Ok(Node { cfg, identity, engine: Arc::new(engine) })
    }

    pub fn hello(&self) -> HelloInfo {
        HelloInfo::local(&self.identity, &self.cfg.device_name, &self.cfg.root().to_string_lossy())
    }

    pub fn peer_row(&self) -> Result<Option<PeerRow>> {
        Ok(self.engine.store().peers()?.into_iter().last())
    }

    /// Peer address: configured address, Bonjour, or the address seen at pairing.
    pub fn peer_addr(&self, row: &PeerRow) -> Result<SocketAddr> {
        if let Some(a) = &self.cfg.peer.address {
            return a.to_socket_addrs()?.next().with_context(|| format!("cannot resolve {a}"));
        }
        if self.cfg.peer.discovery {
            if let Ok(Some(a)) = discovery::find(&row.device, Duration::from_secs(3)) {
                let _ = self.engine.store().set_peer_addr(&row.device, &a.to_string());
                return Ok(a);
            }
        }
        if !row.addr.is_empty() {
            return row.addr.parse().with_context(|| format!("invalid stored peer address {}", row.addr));
        }
        bail!("peer address unknown: set [peer] address in the configuration")
    }

    pub fn connect_peer(&self) -> Result<NetPeer> {
        let row = self.peer_row()?.context("not paired: run `reposync pair` first")?;
        let addr = self.peer_addr(&row)?;
        let peer = NetPeer::new(addr, self.identity.clone(), row.pubkey.clone(), self.hello());
        peer.ping().with_context(|| format!("peer {} at {addr} not reachable", row.name))?;
        Ok(peer)
    }

    /// Capture, pull everything that differs and tell the peer what changed.
    pub fn sync_once(&self, peer: &NetPeer, units: Option<&[String]>, dry_run: bool) -> Result<Vec<PullReport>> {
        if !dry_run {
            self.engine.scan_all(false)?;
            // Let the peer pull our changes first (its pulls are independent).
            let names: Vec<String> = self.engine.local_roots()?.into_iter().map(|r| r.name).collect();
            let _ = peer.notify_changed(names);
        }
        let reports = self.engine.sync_from(peer, units, PullOptions { dry_run })?;
        if !dry_run {
            let changed: Vec<String> =
                reports.iter().filter(|r| r.outcome == Outcome::Applied).map(|r| r.unit.clone()).collect();
            if !changed.is_empty() {
                let _ = peer.notify_changed(changed);
            }
        }
        Ok(reports)
    }

    /// Status of all units as JSON (shared by the CLI and the IPC socket).
    pub fn status(&self, remote: Option<&HashMap<String, Hash>>, dirty: &BTreeSet<String>) -> Result<Value> {
        let store = self.engine.store();
        let mut units = Vec::new();
        for u in store.units()? {
            let (live, tombs) = store.entry_count(&u.name)?;
            let conflicts = store.open_conflict_count(&u.name)?;
            let remote_root = remote.and_then(|m| m.get(&u.name));
            let state = if u.paused {
                "paused".to_string()
            } else if conflicts > 0 {
                "conflict".to_string()
            } else if dirty.contains(&u.name) {
                "waiting for quiet".to_string()
            } else if let (Some(r), Some(l)) = (remote_root, u.root) {
                if *r == l {
                    "in sync".into()
                } else {
                    "differs".into()
                }
            } else if u.status.is_empty() {
                "unknown".into()
            } else {
                u.status.clone()
            };
            units.push(json!({
                "name": u.name,
                "kind": u.kind.as_str(),
                "state": state,
                "root": u.root.map(|h| h.short()),
                "remote_root": remote_root.map(|h| h.short()),
                "paused": u.paused,
                "prefer": u.prefer.as_str(),
                "conflicts": conflicts,
                "entries": live,
                "tombstones": tombs,
                "last_sync": if u.synced_ms > 0 { Some(reposync_fs::time::rfc3339(u.synced_ms)) } else { None },
            }));
        }
        let peer = store.peers()?.into_iter().last().map(|p| {
            json!({"device": p.device, "name": p.name, "address": p.addr, "paired": reposync_fs::time::rfc3339(p.paired_ms)})
        });
        Ok(json!({
            "device": self.identity.device_id(),
            "name": self.cfg.device_name,
            "version": reposync_net::build_version(),
            "root": self.cfg.root(),
            "mode": self.cfg.mode().as_str(),
            "case_insensitive": self.engine.is_case_insensitive(),
            "peer": peer,
            "units": units,
        }))
    }
}

#[derive(Default)]
struct Dirty {
    first: Option<Instant>,
    last: Option<Instant>,
    window_start: Option<Instant>,
    window_events: u32,
    burst: bool,
    gen: u64,
}

#[derive(Default)]
struct PeerState {
    online: bool,
    last_contact_ms: i64,
    last_error: String,
    remote_roots: HashMap<String, Hash>,
}

pub(crate) struct Shared {
    dirty: Mutex<HashMap<String, Dirty>>,
    pending: Mutex<HashMap<String, Instant>>,
    wake: (Mutex<bool>, Condvar),
    stop: AtomicBool,
    paused: AtomicBool,
    units: RwLock<Vec<String>>,
    peer: Mutex<Option<Arc<NetPeer>>>,
    peer_state: Mutex<PeerState>,
    force_ae: AtomicBool,
}

impl Shared {
    fn notify(&self) {
        let (m, c) = &self.wake;
        *m.lock().unwrap_or_else(|e| e.into_inner()) = true;
        c.notify_all();
    }

    fn mark_dirty(&self, unit: &str) {
        let now = Instant::now();
        let mut d = self.dirty.lock().unwrap_or_else(|e| e.into_inner());
        let e = d.entry(unit.to_string()).or_default();
        e.first.get_or_insert(now);
        e.last = Some(now);
        e.gen += 1;
        match e.window_start {
            Some(ws) if now.duration_since(ws) < Duration::from_secs(1) => e.window_events += 1,
            _ => {
                e.burst = e.window_events > 200;
                e.window_start = Some(now);
                e.window_events = 1;
            }
        }
        if e.window_events > 200 {
            e.burst = true;
        }
    }

    fn dirty_gen(&self, unit: &str) -> Option<u64> {
        self.dirty.lock().unwrap_or_else(|e| e.into_inner()).get(unit).map(|d| d.gen)
    }

    fn dirty_set(&self) -> BTreeSet<String> {
        self.dirty.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect()
    }
}

/// The background service.
pub struct Daemon {
    node: Arc<Node>,
    shared: Arc<Shared>,
    _server: Server,
    _watcher: notify::RecommendedWatcher,
    _advertiser: Option<discovery::Advertiser>,
    scheduler: Option<std::thread::JoinHandle<()>>,
    socket: PathBuf,
}

static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

impl Daemon {
    /// Start all threads. Returns once the daemon is running.
    pub fn start(node: Node) -> Result<Daemon> {
        let node = Arc::new(node);
        let shared = Arc::new(Shared {
            dirty: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            wake: (Mutex::new(false), Condvar::new()),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(node.engine.store().kv("paused_all")?.as_deref() == Some("1")),
            units: RwLock::new(Vec::new()),
            peer: Mutex::new(None),
            peer_state: Mutex::new(PeerState::default()),
            force_ae: AtomicBool::new(true),
        });

        // Startup capture replaces the FSEvents event-id replay (ADR-0004).
        tracing::info!(root = %node.cfg.root().display(), "initial scan");
        node.engine.scan_all(false)?;
        refresh_units(&node, &shared);

        // Protocol server: only the paired key may connect.
        let n2 = node.clone();
        let authorize: reposync_net::server::Authorizer = Arc::new(move |key: &[u8]| {
            n2.engine.store().peers().map(|p| p.iter().any(|p| p.pubkey == key)).unwrap_or(false)
        });
        let (n3, s3) = (node.clone(), shared.clone());
        let handler: reposync_net::server::Handler = Arc::new(move |peer, req| {
            {
                let mut ps = s3.peer_state.lock().unwrap_or_else(|e| e.into_inner());
                ps.online = true;
                ps.last_contact_ms = reposync_fs::time::now_ms();
            }
            if let Request::Changed { units } = &req {
                tracing::debug!(peer = %peer.device, ?units, "peer reports changes");
                let mut p = s3.pending.lock().unwrap_or_else(|e| e.into_inner());
                for u in units {
                    p.insert(u.clone(), Instant::now());
                }
                drop(p);
                s3.notify();
            }
            n3.engine.handle(req)
        });
        let server = Server::start(node.cfg.listen, node.identity.clone(), node.hello(), authorize, handler)?;
        tracing::info!(addr = %server.local_addr(), "listening");
        let advertiser = if node.cfg.peer.discovery {
            discovery::Advertiser::start(&node.identity.device_id(), server.local_addr().port())
                .map_err(|e| tracing::warn!(error = %e, "mDNS advertisement unavailable"))
                .ok()
        } else {
            None
        };

        let watcher = start_watcher(&node, &shared)?;

        let socket = crate::ipc::socket_path(&node.cfg.state_dir());
        crate::ipc::serve(&socket, node.clone(), shared.clone())?;

        let (n4, s4) = (node.clone(), shared.clone());
        let scheduler =
            std::thread::Builder::new().name("reposync-scheduler".into()).spawn(move || scheduler(n4, s4))?;

        Ok(Daemon {
            node,
            shared,
            _server: server,
            _watcher: watcher,
            _advertiser: advertiser,
            scheduler: Some(scheduler),
            socket,
        })
    }

    pub fn node(&self) -> &Arc<Node> {
        &self.node
    }

    pub fn server_addr(&self) -> SocketAddr {
        self._server.local_addr()
    }

    /// Block until SIGINT/SIGTERM.
    pub fn run_until_signal(self) -> Result<()> {
        unsafe {
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        }
        while !SIGNALLED.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
        }
        tracing::info!("shutting down");
        self.stop();
        Ok(())
    }

    pub fn stop(mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.notify();
        if let Some(t) = self.scheduler.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn refresh_units(node: &Node, shared: &Shared) {
    if let Ok(u) = node.engine.store().units() {
        *shared.units.write().unwrap_or_else(|e| e.into_inner()) = u.into_iter().map(|u| u.name).collect();
    }
}

/// Map an absolute path reported by the watcher to its unit.
fn unit_of(root: &Path, canon_root: &Path, units: &[String], abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).or_else(|_| abs.strip_prefix(canon_root)).ok()?;
    let rel = rel.to_str()?;
    if rel.is_empty() || rel == reposync_fs::PRIVATE_DIR || rel.starts_with(".reposync/") {
        return None;
    }
    let rel = reposync_core::path::nfc(rel);
    let best = units
        .iter()
        .filter(|u| u.as_str() != reposync_core::entry::ROOT_UNIT && reposync_core::path::is_within(&rel, u))
        .max_by_key(|u| u.len())
        .cloned();
    best.or_else(|| {
        let first = rel.split('/').next().unwrap_or("");
        if rel.contains('/') || root.join(first).is_dir() {
            Some(first.to_string())
        } else {
            Some(reposync_core::entry::ROOT_UNIT.to_string())
        }
    })
}

fn start_watcher(node: &Arc<Node>, shared: &Arc<Shared>) -> Result<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    let root = node.cfg.root();
    let canon = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let s = shared.clone();
    let (r2, c2) = (root.clone(), canon.clone());
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
        Ok(ev) => {
            // Reads (including RepoSync's own scans and hashing) are not
            // changes; only a close after writing is.
            if let notify::EventKind::Access(a) = ev.kind {
                if a != notify::event::AccessKind::Close(notify::event::AccessMode::Write) {
                    return;
                }
            }
            if ev.need_rescan() {
                let all = s.units.read().unwrap_or_else(|e| e.into_inner()).clone();
                for u in all {
                    s.mark_dirty(&u);
                }
            }
            let units = s.units.read().unwrap_or_else(|e| e.into_inner()).clone();
            let mut any = false;
            for p in &ev.paths {
                if let Some(u) = unit_of(&r2, &c2, &units, p) {
                    s.mark_dirty(&u);
                    any = true;
                }
            }
            if any {
                s.notify();
            }
        }
        Err(e) => tracing::warn!(error = %e, "watcher error"),
    })?;
    w.watch(&root, RecursiveMode::Recursive).with_context(|| format!("watch {}", root.display()))?;
    Ok(w)
}

fn peer(node: &Node, shared: &Shared) -> Option<Arc<NetPeer>> {
    if let Some(p) = shared.peer.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return Some(p);
    }
    match node.connect_peer() {
        Ok(p) => {
            let p = Arc::new(p);
            if let Some(h) = p.remote_hello() {
                tracing::info!(peer = %h.name, addr = %p.addr(), "connected to peer");
            }
            *shared.peer.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.clone());
            let mut ps = shared.peer_state.lock().unwrap_or_else(|e| e.into_inner());
            ps.online = true;
            ps.last_error.clear();
            ps.last_contact_ms = reposync_fs::time::now_ms();
            Some(p)
        }
        Err(e) => {
            let mut ps = shared.peer_state.lock().unwrap_or_else(|e| e.into_inner());
            if ps.online || ps.last_error.is_empty() {
                tracing::warn!(error = %format!("{e:#}"), "peer unavailable");
            }
            ps.online = false;
            ps.last_error = format!("{e:#}");
            None
        }
    }
}

fn peer_failed(shared: &Shared, e: &anyhow::Error) {
    tracing::warn!(error = %format!("{e:#}"), "peer request failed");
    *shared.peer.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let mut ps = shared.peer_state.lock().unwrap_or_else(|e| e.into_inner());
    ps.online = false;
    ps.last_error = format!("{e:#}");
}

fn scheduler(node: Arc<Node>, shared: Arc<Shared>) {
    let cfg = node.cfg.sync.clone();
    let mut last_ae: Option<Instant> = None;
    let mut last_verify = Instant::now();
    let mut last_rescan = Instant::now();
    let mut last_housekeeping: Option<Instant> = None;
    let tick = Duration::from_millis(100);
    while !shared.stop.load(Ordering::SeqCst) {
        {
            let (m, c) = &shared.wake;
            let mut woke = m.lock().unwrap_or_else(|e| e.into_inner());
            if !*woke {
                let (g, _) = c.wait_timeout(woke, tick).unwrap_or_else(|e| e.into_inner());
                woke = g;
            }
            *woke = false;
        }
        if shared.paused.load(Ordering::SeqCst) {
            continue;
        }
        let now = Instant::now();

        // 1. Capture units that became quiet.
        let ready: Vec<(String, u64, bool)> = {
            let d = shared.dirty.lock().unwrap_or_else(|e| e.into_inner());
            d.iter()
                .filter_map(|(u, e)| {
                    let quiet = Duration::from_millis(if e.burst { cfg.max_quiet_ms } else { cfg.quiet_ms });
                    let last = e.last?;
                    let overdue = e.first.is_some_and(|f| now.duration_since(f) > Duration::from_secs(cfg.max_wait_s));
                    (now.duration_since(last) >= quiet || overdue).then(|| (u.clone(), e.gen, overdue))
                })
                .collect()
        };
        let mut changed = Vec::new();
        for (unit, gen, overdue) in ready {
            let still = || shared.dirty_gen(&unit) == Some(gen);
            let opts = ScanOptions { full: false, still_quiet: if overdue { None } else { Some(&still) } };
            match node.engine.scan_unit(&unit, &opts) {
                Ok(r) if r.deferred.is_some() => {
                    tracing::debug!(%unit, reason = r.deferred.as_deref().unwrap_or(""), "capture deferred");
                    // Keep it dirty; retry after the quiet period.
                    let mut d = shared.dirty.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(e) = d.get_mut(&unit) {
                        if e.gen == gen {
                            e.last = Some(Instant::now());
                        }
                    }
                }
                Ok(r) => {
                    {
                        let mut d = shared.dirty.lock().unwrap_or_else(|e| e.into_inner());
                        if d.get(&unit).is_some_and(|e| e.gen == gen) {
                            d.remove(&unit);
                        }
                    }
                    for n in &r.nested {
                        shared.mark_dirty(n);
                    }
                    if r.changed > 0 {
                        changed.push(unit.clone());
                    }
                }
                Err(e) => {
                    tracing::warn!(%unit, error = %format!("{e:#}"), "capture failed");
                    shared.dirty.lock().unwrap_or_else(|e| e.into_inner()).remove(&unit);
                }
            }
        }
        if !changed.is_empty() {
            refresh_units(&node, &shared);
            if let Some(p) = peer(&node, &shared) {
                if let Err(e) = p.notify_changed(changed) {
                    peer_failed(&shared, &e);
                }
            }
        }

        // 2. Pull units the peer reported as changed (once they are not dirty here).
        let due: Vec<String> = {
            let dirty = shared.dirty_set();
            let mut p = shared.pending.lock().unwrap_or_else(|e| e.into_inner());
            let due: Vec<String> =
                p.iter().filter(|(u, at)| !dirty.contains(*u) && **at <= now).map(|(u, _)| u.clone()).collect();
            for u in &due {
                p.remove(u);
            }
            due
        };
        let ae_due = shared.force_ae.swap(false, Ordering::SeqCst)
            || last_ae.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(cfg.anti_entropy_s));
        if !due.is_empty() || ae_due {
            if let Some(p) = peer(&node, &shared) {
                if ae_due {
                    last_ae = Some(now);
                    if let Err(e) = anti_entropy(&node, &shared, &p) {
                        peer_failed(&shared, &e);
                    }
                } else if let Err(e) = pull(&node, &shared, &p, Some(&due)) {
                    peer_failed(&shared, &e);
                }
            } else if !due.is_empty() {
                // Peer offline: retry with the next anti-entropy round.
                last_ae = last_ae.or(Some(now));
            }
        }

        // 3. Safety net against missed events: periodic stat-only rescan.
        if cfg.rescan_interval_s > 0 && now.duration_since(last_rescan) >= Duration::from_secs(cfg.rescan_interval_s) {
            last_rescan = now;
            match node.engine.scan_all(false) {
                Ok(r) => {
                    let missed: Vec<String> = r.iter().filter(|r| r.changed > 0).map(|r| r.unit.clone()).collect();
                    if !missed.is_empty() {
                        tracing::info!(?missed, "periodic rescan found changes without events");
                        for u in &missed {
                            let _ =
                                node.engine.store().log(u, "drift", "", "periodic rescan found uncaptured changes", "");
                        }
                        refresh_units(&node, &shared);
                        if let Some(p) = peer(&node, &shared) {
                            if let Err(e) = p.notify_changed(missed) {
                                peer_failed(&shared, &e);
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "periodic rescan failed"),
            }
        }

        // 4. Daily verification and housekeeping.
        if now.duration_since(last_verify) >= Duration::from_secs(cfg.verify_interval_h.max(1) * 3600) {
            last_verify = now;
            match node.engine.scan_all(true) {
                Ok(r) => {
                    let silent: usize = r.iter().map(|r| r.silent_changes).sum();
                    tracing::info!(silent, "verification finished");
                }
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "verification failed"),
            }
        }
        if last_housekeeping.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(3600)) {
            last_housekeeping = Some(now);
            let in_sync: Vec<String> = {
                let ps = shared.peer_state.lock().unwrap_or_else(|e| e.into_inner());
                node.engine
                    .local_roots()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|r| ps.remote_roots.get(&r.name) == Some(&r.root))
                    .map(|r| r.name)
                    .collect()
            };
            if let Err(e) = node.engine.housekeeping(&in_sync) {
                tracing::warn!(error = %e, "housekeeping failed");
            }
        }
    }
}

fn pull(node: &Node, shared: &Shared, peer: &NetPeer, units: Option<&[String]>) -> Result<()> {
    let reports = node.engine.sync_from(peer, units, PullOptions::default())?;
    let mut applied = Vec::new();
    for r in &reports {
        match &r.outcome {
            Outcome::Applied => {
                tracing::info!(
                    unit = %r.unit, writes = r.writes, deletes = r.deletes, conflicts = r.conflicts,
                    fetched = r.bytes_fetched, deduped = r.bytes_deduped, converged = r.converged, "applied"
                );
                applied.push(r.unit.clone());
            }
            Outcome::Deferred(why) => {
                tracing::debug!(unit = %r.unit, %why, "pull deferred");
                // A local change is pending: capture it, then retry.
                shared.mark_dirty(&r.unit);
                shared
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(r.unit.clone(), Instant::now() + Duration::from_secs(1));
            }
            Outcome::Failed(e) => tracing::warn!(unit = %r.unit, error = %e, "pull failed"),
            _ => {}
        }
    }
    if !applied.is_empty() {
        refresh_units(node, shared);
        peer.notify_changed(applied)?;
    }
    Ok(())
}

/// Compare all roots with the peer; pull whatever differs (drift repair).
fn anti_entropy(node: &Node, shared: &Shared, peer: &NetPeer) -> Result<()> {
    let remote = peer.roots()?;
    let local: HashMap<String, Hash> = node.engine.local_roots()?.into_iter().map(|r| (r.name, r.root)).collect();
    let dirty = shared.dirty_set();
    let pending: BTreeSet<String> = shared.pending.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
    let mut differ = Vec::new();
    {
        let mut ps = shared.peer_state.lock().unwrap_or_else(|e| e.into_inner());
        ps.remote_roots = remote.iter().map(|r| (r.name.clone(), r.root)).collect();
        ps.online = true;
        ps.last_contact_ms = reposync_fs::time::now_ms();
    }
    for r in &remote {
        if local.get(&r.name) != Some(&r.root) && !dirty.contains(&r.name) {
            if !pending.contains(&r.name) {
                let _ = node.engine.store().log(&r.name, "drift", "", "root differs from peer during anti-entropy", "");
            }
            differ.push(r.name.clone());
        }
    }
    if !differ.is_empty() {
        pull(node, shared, peer, Some(&differ))?;
    }
    let _ = node.engine.recover();
    Ok(())
}

// ---- used by the IPC layer ---------------------------------------------------

impl Daemon {
    pub(crate) fn status_json(node: &Node, shared: &Shared) -> Result<Value> {
        let ps = shared.peer_state.lock().unwrap_or_else(|e| e.into_inner());
        let remote = ps.remote_roots.clone();
        let online = ps.online;
        let last = ps.last_contact_ms;
        let err = ps.last_error.clone();
        drop(ps);
        let mut v = node.status(Some(&remote), &shared.dirty_set())?;
        v["daemon"] = json!(true);
        v["paused"] = json!(shared.paused.load(Ordering::SeqCst));
        v["peer_online"] = json!(online);
        v["peer_last_contact"] = json!(if last > 0 { Some(reposync_fs::time::rfc3339(last)) } else { None });
        v["peer_error"] = json!(if err.is_empty() { None } else { Some(err) });
        let peer_version = shared
            .peer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|p| p.remote_hello())
            .map(|h| h.version);
        v["peer_version"] = json!(peer_version);
        Ok(v)
    }

    pub(crate) fn ipc_sync(node: &Node, shared: &Shared, units: Option<Vec<String>>, dry_run: bool) -> Result<Value> {
        let p = peer(node, shared).with_context(|| {
            let ps = shared.peer_state.lock().unwrap_or_else(|e| e.into_inner());
            format!("peer not reachable: {}", ps.last_error)
        })?;
        let r = node.sync_once(&p, units.as_deref(), dry_run);
        if let Err(e) = &r {
            peer_failed(shared, e);
        }
        refresh_units(node, shared);
        Ok(serde_json::to_value(r?)?)
    }

    pub(crate) fn set_paused(node: &Node, shared: &Shared, paused: bool) -> Result<()> {
        shared.paused.store(paused, Ordering::SeqCst);
        node.engine.store().set_kv("paused_all", if paused { "1" } else { "0" })?;
        if !paused {
            shared.force_ae.store(true, Ordering::SeqCst);
            shared.notify();
        }
        Ok(())
    }

    pub(crate) fn kick(shared: &Shared) {
        shared.force_ae.store(true, Ordering::SeqCst);
        shared.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_paths_to_units() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("newdir")).unwrap();
        let units = vec![".".to_string(), "proj".into(), "clients".into(), "clients/acme".into()];
        let m = |p: &str| unit_of(root, root, &units, &root.join(p));
        assert_eq!(m("proj/src/a.rs").as_deref(), Some("proj"));
        assert_eq!(m("clients/acme/.git/index").as_deref(), Some("clients/acme"));
        assert_eq!(m("clients/notes.md").as_deref(), Some("clients"));
        assert_eq!(m("loose.txt").as_deref(), Some("."));
        assert_eq!(m("newdir").as_deref(), Some("newdir"));
        assert_eq!(m("other/x").as_deref(), Some("other"));
        assert_eq!(m(".reposync/staging/x"), None);
    }
}
