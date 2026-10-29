//! The running node: identity, engine, peer, and the daemon threads
//! (protocol server, watcher, scheduler, IPC).

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use macsync_core::proto::Request;
use macsync_core::Hash;
use macsync_engine::{Engine, Outcome, PeerApi, PullOptions, PullQueue, PullReport, ScanOptions};
use macsync_net::client::HelloInfo;
use macsync_net::{discovery, Identity, NetPeer, Server};
use macsync_store::PeerRow;
use serde_json::{json, Value};

use crate::config::Config;

/// Identity + engine + configuration. Used by the daemon and, when no daemon
/// runs, directly by the CLI.
pub struct Node {
    pub cfg: Config,
    pub identity: Identity,
    pub engine: Arc<Engine>,
    counts: Arc<Counts>,
}

/// `(root when counted, live, tombstones)`.
type Counted = (Option<Hash>, u64, u64);

/// Entry counts per unit for the status. Counting a large unit reads a big
/// part of the database, so the result is kept while the unit's root stays
/// the same; in the daemon it is computed in the background on a connection
/// of its own and never holds up the status, sync or the manager.
#[derive(Default)]
struct Counts {
    cache: Mutex<HashMap<String, Counted>>,
    background: AtomicBool,
    running: AtomicBool,
    conn: Mutex<Option<macsync_store::Store>>,
}

impl Node {
    pub fn open(cfg: Config) -> Result<Node> {
        let state = cfg.state_dir();
        let identity = Identity::load_or_create(&state)?;
        let engine = Engine::open(cfg.engine_config(&identity.device_id())?)?;
        Ok(Node { cfg, identity, engine: Arc::new(engine), counts: Arc::default() })
    }

    /// `(live, tombstones)` per unit; `None` while a background count runs.
    fn entry_counts(
        &self,
        store: &macsync_store::Store,
        units: &[macsync_store::UnitRow],
    ) -> Result<HashMap<String, Option<(u64, u64)>>> {
        let cache = self.counts.cache.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let fresh = |u: &macsync_store::UnitRow| {
            cache.get(&u.name).filter(|c| c.0.is_some() && c.0 == u.root).map(|c| (c.1, c.2))
        };
        let missing: Vec<(String, Option<Hash>)> =
            units.iter().filter(|u| fresh(u).is_none()).map(|u| (u.name.clone(), u.root)).collect();
        if !self.counts.background.load(Ordering::SeqCst) {
            // CLI without daemon: count right here.
            let mut out = HashMap::new();
            for u in units {
                out.insert(u.name.clone(), Some(store.entry_count(&u.name)?));
            }
            return Ok(out);
        }
        if !missing.is_empty() && !self.counts.running.swap(true, Ordering::SeqCst) {
            let counts = self.counts.clone();
            let db = macsync_store::db_path(&self.cfg.state_dir())?;
            std::thread::Builder::new().name("macsync-counts".into()).spawn(move || {
                let mut conn = counts.conn.lock().unwrap_or_else(|e| e.into_inner());
                if conn.is_none() {
                    *conn = macsync_store::Store::open(&db)
                        .map_err(|e| tracing::warn!(error = %format!("{e:#}"), "count connection"))
                        .ok();
                }
                if let Some(c) = conn.as_ref() {
                    for (unit, root) in missing {
                        if let Ok((live, tombs)) = c.entry_count(&unit) {
                            counts.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(unit, (root, live, tombs));
                        }
                    }
                }
                counts.running.store(false, Ordering::SeqCst);
            })?;
        }
        // Stale numbers are better than none while the new ones are counted.
        Ok(units.iter().map(|u| (u.name.clone(), cache.get(&u.name).map(|c| (c.1, c.2)))).collect())
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
            return macsync_net::lan::resolve(a);
        }
        if self.cfg.peer.discovery {
            let blinding = discovery::Blinding::new(&self.identity, &row.pubkey)?;
            if let Ok(Some(a)) = discovery::find(&blinding, Duration::from_secs(3)) {
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
        let row = self.peer_row()?.context("not paired: run `macsync pair` first")?;
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
        let rows = store.units()?;
        let counts = self.entry_counts(&store, &rows)?;
        let mut units = Vec::new();
        for u in rows {
            let (live, tombs) = match counts.get(&u.name).copied().flatten() {
                Some((l, t)) => (Some(l), Some(t)),
                None => (None, None),
            };
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
                "last_sync": if u.synced_ms > 0 { Some(macsync_fs::time::rfc3339(u.synced_ms)) } else { None },
            }));
        }
        let peer = store.peers()?.into_iter().last().map(|p| {
            json!({"device": p.device, "name": p.name, "address": p.addr, "paired": macsync_fs::time::rfc3339(p.paired_ms)})
        });
        Ok(json!({
            "device": self.identity.device_id(),
            "name": self.cfg.device_name,
            "version": macsync_net::build_version(),
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
    /// A second connection for the manager's status requests, so they never
    /// wait behind a transfer on `peer`.
    browse_peer: Mutex<Option<Arc<NetPeer>>>,
    /// Units the user wants synchronised next, with the path they chose.
    priority: Mutex<Vec<Priority>>,
    peer_state: Mutex<PeerState>,
    force_ae: AtomicBool,
    /// Listening port and Bonjour announcement; only while a peer is paired.
    exposure: Mutex<Option<Exposure>>,
    network: Mutex<NetworkState>,
}

/// The current network (checked every few seconds) and whether the daemon
/// hides in it.
#[derive(Default)]
struct NetworkState {
    id: Option<String>,
    checked: Option<Instant>,
    known: bool,
    hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Priority {
    unit: String,
    path: String,
}

/// [`Shared::priority`] as the engine's pull queue.
struct SharedQueue<'a>(&'a Shared);

impl PullQueue for SharedQueue<'_> {
    fn next(&self, done: &BTreeSet<String>) -> Option<String> {
        let q = self.0.priority.lock().unwrap_or_else(|e| e.into_inner());
        q.iter().find(|p| !done.contains(&p.unit)).map(|p| p.unit.clone())
    }
    fn done(&self, unit: &str) {
        self.0.priority.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| p.unit != unit);
    }
}

/// What the daemon shows to the network.
struct Exposure {
    peer_key: Vec<u8>,
    server: Server,
    _advertiser: Option<discovery::Advertiser>,
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

    /// Put `units` at the front of the queue (`path`: what the user chose).
    fn prioritize(&self, units: &[String], path: &str) {
        let mut q = self.priority.lock().unwrap_or_else(|e| e.into_inner());
        q.retain(|p| !units.contains(&p.unit));
        for (i, u) in units.iter().enumerate() {
            q.insert(i, Priority { unit: u.clone(), path: path.to_string() });
        }
        q.truncate(256);
        drop(q);
        self.notify();
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
    _watcher: notify::RecommendedWatcher,
    scheduler: Option<std::thread::JoinHandle<()>>,
    socket: PathBuf,
}

static SIGNALLED: AtomicBool = AtomicBool::new(false);

/// How long a stopping daemon waits for the sync in progress (the app sends
/// SIGTERM after 3 s).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

impl Daemon {
    /// Start all threads. Returns once the daemon is running.
    pub fn start(node: Node) -> Result<Daemon> {
        node.counts.background.store(true, Ordering::SeqCst);
        let node = Arc::new(node);
        let shared = Arc::new(Shared {
            dirty: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            wake: (Mutex::new(false), Condvar::new()),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(node.engine.store().kv("paused_all")?.as_deref() == Some("1")),
            units: RwLock::new(Vec::new()),
            peer: Mutex::new(None),
            browse_peer: Mutex::new(None),
            priority: Mutex::new(Vec::new()),
            peer_state: Mutex::new(PeerState::default()),
            force_ae: AtomicBool::new(true),
            exposure: Mutex::new(None),
            network: Mutex::new(NetworkState::default()),
        });

        // Startup capture replaces the FSEvents event-id replay (ADR-0004).
        tracing::info!(root = %node.cfg.root().display(), "initial scan");
        node.engine.scan_all(false)?;
        refresh_units(&node, &shared);

        // Protocol server: only while paired, and only for the paired key.
        update_exposure(&node, &shared);

        let watcher = start_watcher(&node, &shared)?;

        let socket = crate::ipc::socket_path(&node.cfg.state_dir());
        crate::ipc::serve(&socket, node.clone(), shared.clone())?;

        let (n4, s4) = (node.clone(), shared.clone());
        let scheduler =
            std::thread::Builder::new().name("macsync-scheduler".into()).spawn(move || scheduler(n4, s4))?;

        Ok(Daemon { node, shared, _watcher: watcher, scheduler: Some(scheduler), socket })
    }

    pub fn node(&self) -> &Arc<Node> {
        &self.node
    }

    /// Listening address; `None` while no peer is paired.
    pub fn server_addr(&self) -> Option<SocketAddr> {
        self.shared.exposure.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|x| x.server.local_addr())
    }

    /// Block until SIGINT/SIGTERM.
    pub fn run_until_signal(self) -> Result<()> {
        unsafe {
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        }
        // Started by the menu bar app: stop when the app is gone (crash,
        // force quit), so no orphaned daemon keeps running.
        let parent: Option<libc::pid_t> = std::env::var("MACSYNC_PARENT_PID").ok().and_then(|p| p.parse().ok());
        while !SIGNALLED.load(Ordering::SeqCst) {
            if parent.is_some_and(|p| unsafe { libc::getppid() } != p) {
                tracing::info!("parent process ended");
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        tracing::info!("shutting down");
        self.stop_within(SHUTDOWN_GRACE);
        Ok(())
    }

    pub fn stop(self) {
        self.stop_within(Duration::MAX);
    }

    /// Stop and wait at most `grace` for the sync in progress. A large unit
    /// can take minutes; meanwhile the updated app would find the old daemon
    /// still holding the socket. Leaving earlier is safe: an interrupted
    /// transaction is completed on the next start (transaction journal).
    pub fn stop_within(mut self, grace: Duration) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.notify();
        // The server's handler holds `shared`: drop it explicitly.
        self.shared.exposure.lock().unwrap_or_else(|e| e.into_inner()).take();
        let _ = std::fs::remove_file(&self.socket);
        if let Some(t) = self.scheduler.take() {
            let deadline = Instant::now() + grace.min(Duration::from_secs(86_400));
            while !t.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if t.is_finished() {
                let _ = t.join();
            } else {
                tracing::info!("sync still running; it continues after the next start");
            }
        }
    }
}

/// Open the port (and the Bonjour announcement) for the paired peer, close it
/// when there is none, and restart it after a new pairing.
fn update_exposure(node: &Arc<Node>, shared: &Arc<Shared>) {
    let hidden = check_network(node, shared);
    let key = node.peer_row().ok().flatten().map(|r| r.pubkey).filter(|_| !hidden);
    let mut exp = shared.exposure.lock().unwrap_or_else(|e| e.into_inner());
    if exp.as_ref().map(|x| &x.peer_key) == key.as_ref() {
        return;
    }
    if exp.take().is_some() {
        tracing::info!("stopped listening");
    }
    let Some(key) = key else {
        if hidden {
            tracing::info!("unknown network: not listening, not announcing");
        } else {
            tracing::info!("not paired: not listening");
        }
        return;
    };
    match open_exposure(node, shared, key) {
        Ok(x) => *exp = Some(x),
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "cannot listen"),
    }
}

/// Refresh the network state; a network where the peer is reached becomes
/// known. Returns true if the daemon should hide.
fn check_network(node: &Node, shared: &Shared) -> bool {
    let online = shared.peer_state.lock().unwrap_or_else(|e| e.into_inner()).online;
    let mut net = shared.network.lock().unwrap_or_else(|e| e.into_inner());
    if net.checked.is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) {
        net.id = crate::network::current();
        net.checked = Some(Instant::now());
    }
    let store = node.engine.store();
    if let Some(id) = &net.id {
        if online && crate::network::remember(&store, id).unwrap_or(false) {
            tracing::info!("network remembered: the peer is reachable here");
        }
    }
    net.known = net.id.as_deref().is_some_and(|id| crate::network::is_known(&store, id).unwrap_or(false));
    net.hidden = node.cfg.peer.hide_in_unknown_networks && !net.known;
    net.hidden
}

fn open_exposure(node: &Arc<Node>, shared: &Arc<Shared>, peer_key: Vec<u8>) -> Result<Exposure> {
    let (n3, s3) = (node.clone(), shared.clone());
    let handler: macsync_net::server::Handler = Arc::new(move |peer, req| {
        {
            let mut ps = s3.peer_state.lock().unwrap_or_else(|e| e.into_inner());
            ps.online = true;
            ps.last_contact_ms = macsync_fs::time::now_ms();
        }
        if let Request::Prioritize { units } = &req {
            tracing::info!(peer = %peer.device, ?units, "peer puts units first");
            let units: Vec<String> =
                units.iter().filter(|u| macsync_engine::validate_unit_name(u).is_ok()).take(64).cloned().collect();
            s3.prioritize(&units, "");
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
    let server = Server::start(node.cfg.listen, node.identity.clone(), node.hello(), peer_key.clone(), handler)?;
    tracing::info!(addr = %server.local_addr(), "listening");
    let advertiser = if node.cfg.peer.discovery {
        discovery::Blinding::new(&node.identity, &peer_key)
            .and_then(|b| discovery::Advertiser::start(b, server.local_addr().port()))
            .map_err(|e| tracing::warn!(error = %e, "mDNS advertisement unavailable"))
            .ok()
    } else {
        None
    };
    Ok(Exposure { peer_key, server, _advertiser: advertiser })
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
    if rel.is_empty() || macsync_fs::is_private(rel) {
        return None;
    }
    let rel = macsync_core::path::nfc(rel);
    let best = units
        .iter()
        .filter(|u| u.as_str() != macsync_core::entry::ROOT_UNIT && macsync_core::path::is_within(&rel, u))
        .max_by_key(|u| u.len())
        .cloned();
    best.or_else(|| {
        let first = rel.split('/').next().unwrap_or("");
        if rel.contains('/') || root.join(first).is_dir() {
            Some(first.to_string())
        } else {
            Some(macsync_core::entry::ROOT_UNIT.to_string())
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
            // Reads (including MacSync's own scans and hashing) are not
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
            ps.last_contact_ms = macsync_fs::time::now_ms();
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

/// The manager's own connection, opened next to the sync connection while
/// the peer is online (never a connect attempt of its own otherwise).
fn browse_peer(shared: &Shared) -> Option<Arc<NetPeer>> {
    let main = shared.peer.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
    let mut b = shared.browse_peer.lock().unwrap_or_else(|e| e.into_inner());
    if b.as_ref().is_none_or(|p| p.addr() != main.addr()) {
        *b = Some(Arc::new(main.sibling()));
    }
    b.clone()
}

fn peer_failed(shared: &Shared, e: &anyhow::Error) {
    tracing::warn!(error = %format!("{e:#}"), "peer request failed");
    *shared.peer.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *shared.browse_peer.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
    let mut last_exposure = Instant::now();
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
        // Pairing (`macsync pair`) changes the peer behind our back.
        if last_exposure.elapsed() >= Duration::from_secs(2) {
            last_exposure = Instant::now();
            update_exposure(&node, &shared);
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

        // 2. Units the user put first (rounds below also check the queue
        // between units, so this only starts them when nothing else runs).
        let queued = !shared.priority.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
        // Only over an open connection: offline, the next anti-entropy round
        // reconnects and takes the queue first.
        let open = shared.peer.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let (true, Some(p)) = (queued, open) {
            if let Err(e) = pull(&node, &shared, &p, Some(&[])) {
                peer_failed(&shared, &e);
            }
        }

        // 3. Pull units the peer reported as changed (once they are not dirty here).
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
    let reports = node.engine.sync_from_queue(peer, units, PullOptions::default(), &SharedQueue(shared))?;
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
        ps.last_contact_ms = macsync_fs::time::now_ms();
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
        v["peer_last_contact"] = json!(if last > 0 { Some(macsync_fs::time::rfc3339(last)) } else { None });
        v["peer_error"] = json!(if err.is_empty() { None } else { Some(err) });
        let peer_version = shared
            .peer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|p| p.remote_hello())
            .map(|h| h.version);
        v["peer_version"] = json!(peer_version);
        let net = shared.network.lock().unwrap_or_else(|e| e.into_inner());
        v["network"] = json!({
            "known": net.known,
            "hidden": net.hidden,
            "hide_unknown": node.cfg.peer.hide_in_unknown_networks,
        });
        drop(net);
        let q = shared.priority.lock().unwrap_or_else(|e| e.into_inner());
        v["priority"] = json!(q.iter().map(|p| json!({ "unit": p.unit, "path": p.path })).collect::<Vec<_>>());
        drop(q);
        v["transfer"] = json!(node.engine.transfer());
        let (served, served_ms) = node.engine.served();
        v["served_bytes"] = json!(served);
        v["served_ms"] = json!(served_ms);
        Ok(v)
    }

    /// Put the units behind `path` first: the unit containing it and every
    /// unit below it. The peer is told too, so both directions go first.
    pub(crate) fn ipc_prioritize(node: &Node, shared: &Shared, path: &str) -> Result<Value> {
        let path = macsync_core::path::nfc(path.trim_matches('/'));
        let (unit, _) = node.engine.split_path(&path)?;
        let prefix = format!("{path}/");
        let mut units: Vec<String> = unit.into_iter().collect();
        let mut below: Vec<String> =
            node.engine.store().units()?.into_iter().map(|u| u.name).filter(|n| n.starts_with(&prefix)).collect();
        below.sort();
        for u in below {
            if !units.contains(&u) {
                units.push(u);
            }
        }
        if units.is_empty() {
            // The sync root itself: every unit is "first", nothing to reorder.
            return Ok(json!({ "units": [] }));
        }
        tracing::info!(%path, ?units, "put first");
        shared.prioritize(&units, &path);
        if let Some(p) = browse_peer(shared) {
            if let Err(e) = p.prioritize(units.clone()) {
                tracing::debug!(error = %format!("{e:#}"), "peer not told about the priority");
                *shared.browse_peer.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
        Ok(json!({ "units": units }))
    }

    /// Never connects: the IPC answer must not wait for discovery or a
    /// connect timeout. Only an existing connection is asked (the sync loop
    /// keeps it up); `local` skips even that.
    pub(crate) fn ipc_browse(
        node: &Node,
        shared: &Shared,
        unit: Option<&str>,
        dir: &str,
        local: bool,
    ) -> Result<Value> {
        let p = if local { None } else { browse_peer(shared) };
        let items = match &p {
            Some(peer_conn) => match node.engine.browse(Some(peer_conn.as_ref() as &dyn PeerApi), unit, dir) {
                Ok(items) => items,
                Err(e) => {
                    // Only this connection: the sync loop notices a lost peer.
                    tracing::debug!(error = %format!("{e:#}"), "browse request failed");
                    *shared.browse_peer.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    node.engine.browse(None, unit, dir)?
                }
            },
            None => node.engine.browse(None, unit, dir)?,
        };
        let name = node.engine.store().peers()?.into_iter().last().map(|r| r.name);
        let online = shared.peer_state.lock().unwrap_or_else(|e| e.into_inner()).online;
        Ok(json!({
            "online": online,
            "peer": name,
            "unit": unit,
            "dir": dir,
            // Whether the items carry the other Mac's state.
            "remote": p.is_some(),
            "items": items,
        }))
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

    /// Remember the current network ("trust this network") and listen again.
    pub(crate) fn trust_network(node: &Arc<Node>, shared: &Arc<Shared>) -> Result<Value> {
        let id = crate::network::current().context("no network (no default gateway)")?;
        crate::network::remember(&node.engine.store(), &id)?;
        shared.network.lock().unwrap_or_else(|e| e.into_inner()).checked = None;
        update_exposure(node, shared);
        Ok(json!("trusted"))
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
        assert_eq!(m(".macsync/staging/x"), None);
    }

    /// No port is open before pairing; it opens once a peer is stored.
    #[test]
    fn listens_only_while_paired() {
        let d = tempfile::tempdir().unwrap();
        let cfg = Config {
            root: d.path().join("root"),
            state_dir: Some(d.path().join("state")),
            listen: "127.0.0.1:0".parse().unwrap(),
            peer: crate::config::PeerConfig {
                address: Some("127.0.0.1:9".into()),
                discovery: false,
                ..Default::default()
            },
            ..Config::default()
        };
        std::fs::create_dir_all(cfg.root()).unwrap();
        let daemon = Daemon::start(Node::open(cfg).unwrap()).unwrap();
        assert_eq!(daemon.server_addr(), None);
        add_test_peer(&daemon);
        let addr = wait_listening(&daemon, 5).expect("listening after pairing");
        assert!(std::net::TcpStream::connect(addr).is_ok());
        daemon.stop();
        assert!(std::net::TcpStream::connect(addr).is_err(), "port still open after stop");
    }

    /// A sync that takes minutes must not keep a stopping daemon alive: the
    /// updated app would find the old one still holding the socket.
    #[test]
    fn stop_does_not_wait_for_a_long_sync() {
        let d = tempfile::tempdir().unwrap();
        let cfg = Config {
            root: d.path().join("root"),
            state_dir: Some(d.path().join("state")),
            listen: "127.0.0.1:0".parse().unwrap(),
            peer: crate::config::PeerConfig { discovery: false, ..Default::default() },
            ..Config::default()
        };
        std::fs::create_dir_all(cfg.root()).unwrap();
        let daemon = Daemon::start(Node::open(cfg).unwrap()).unwrap();
        let socket = daemon.socket.clone();
        assert!(socket.exists());
        // Stand-in for a long sync: the scheduler blocks on the store.
        let node = daemon.node().clone();
        let (held, wait) = std::sync::mpsc::channel();
        let busy = std::thread::spawn(move || {
            let _store = node.engine.store();
            held.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(6));
        });
        wait.recv().unwrap();
        std::thread::sleep(Duration::from_millis(2500)); // scheduler hits the store
        let t = Instant::now();
        daemon.stop_within(Duration::from_millis(300));
        assert!(t.elapsed() < Duration::from_secs(2), "stop took {:?}", t.elapsed());
        assert!(!socket.exists(), "socket left behind");
        busy.join().unwrap();
    }

    fn add_test_peer(daemon: &Daemon) {
        let peer = Identity::generate().unwrap();
        daemon
            .node()
            .engine
            .store()
            .add_peer(&PeerRow {
                device: peer.device_id(),
                name: "Peer".into(),
                pubkey: peer.public.clone(),
                addr: String::new(),
                paired_ms: 0,
            })
            .unwrap();
    }

    fn wait_listening(daemon: &Daemon, secs: u64) -> Option<SocketAddr> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while daemon.server_addr().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        daemon.server_addr()
    }

    /// With `hide_in_unknown_networks`, a paired daemon stays closed in an
    /// unknown network and opens once the network is trusted.
    #[test]
    fn hides_in_unknown_networks() {
        std::env::set_var("MACSYNC_NETWORK_ID", "test-network-1");
        let d = tempfile::tempdir().unwrap();
        let cfg = Config {
            root: d.path().join("root"),
            state_dir: Some(d.path().join("state")),
            listen: "127.0.0.1:0".parse().unwrap(),
            peer: crate::config::PeerConfig {
                address: Some("127.0.0.1:9".into()),
                discovery: false,
                hide_in_unknown_networks: true,
            },
            ..Config::default()
        };
        std::fs::create_dir_all(cfg.root()).unwrap();
        let daemon = Daemon::start(Node::open(cfg).unwrap()).unwrap();
        add_test_peer(&daemon);
        assert_eq!(wait_listening(&daemon, 3), None, "listening in an unknown network");
        let status = Daemon::status_json(&daemon.node, &daemon.shared).unwrap();
        assert_eq!(status["network"]["hidden"], json!(true));
        Daemon::trust_network(&daemon.node, &daemon.shared).unwrap();
        assert!(daemon.server_addr().is_some(), "not listening after trusting the network");
        let status = Daemon::status_json(&daemon.node, &daemon.shared).unwrap();
        assert_eq!(status["network"], json!({"known": true, "hidden": false, "hide_unknown": true}));
        daemon.stop();
    }
}

#[cfg(test)]
mod status_load {
    use super::*;
    use macsync_core::{Entry, Kind, UnitKind};
    use macsync_store::Stored;

    /// Many entries and many status requests at once (the app polls; each
    /// waiting client asks again): the daemon keeps answering, counts arrive
    /// in the background, and a burst never piles up without end.
    #[test]
    fn status_stays_fast_with_many_entries_and_requests() {
        let d = tempfile::tempdir().unwrap();
        let cfg = Config {
            root: d.path().join("root"),
            state_dir: Some(d.path().join("state")),
            listen: "127.0.0.1:0".parse().unwrap(),
            peer: crate::config::PeerConfig {
                address: Some("127.0.0.1:9".into()),
                discovery: false,
                ..Default::default()
            },
            ..Config::default()
        };
        std::fs::create_dir_all(cfg.root().join("big")).unwrap();
        let node = Node::open(cfg).unwrap();
        {
            let st = node.engine.store();
            st.upsert_unit("big", UnitKind::Folder).unwrap();
            st.atomically(|st| {
                for i in 0..300_000 {
                    let entry = Entry::tombstone(&format!("d{}/f{i}", i % 100), Kind::File, Default::default());
                    st.put_entry(
                        "big",
                        &Stored { entry, disk: String::new(), stat: Default::default(), changed_ms: 0 },
                    )?;
                }
                Ok(())
            })
            .unwrap();
        }
        let state = node.cfg.state_dir();
        let daemon = Daemon::start(node).unwrap();

        let t = Instant::now();
        let threads: Vec<_> = (0..64)
            .map(|_| {
                let state = state.clone();
                std::thread::spawn(move || {
                    let t = Instant::now();
                    let r = crate::ipc::call(&state, &crate::ipc::IpcRequest::Status);
                    (t.elapsed(), r.map(|v| v.is_some()).unwrap_or_else(|e| e.to_string().contains("busy")))
                })
            })
            .collect();
        for th in threads {
            let (took, ok) = th.join().unwrap();
            assert!(ok, "status failed");
            assert!(took < Duration::from_secs(10), "a status request took {took:?}");
        }
        eprintln!("64 status requests in {:?}", t.elapsed());

        // The counts arrive in the background.
        let deadline = Instant::now() + Duration::from_secs(30);
        let tombs = loop {
            let v = crate::ipc::call(&state, &crate::ipc::IpcRequest::Status).unwrap().unwrap();
            let big = v["units"].as_array().unwrap().iter().find(|u| u["name"] == "big").cloned().unwrap();
            if let Some(n) = big["tombstones"].as_u64() {
                break n;
            }
            assert!(Instant::now() < deadline, "counts never arrived");
            std::thread::sleep(Duration::from_millis(100));
        };
        assert_eq!(tombs, 300_000);
        daemon.stop();
    }
}
