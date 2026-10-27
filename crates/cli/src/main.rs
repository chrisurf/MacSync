//! `macsync` – command line interface.
//!
//! Mutating commands talk to a running daemon over its control socket; if no
//! daemon runs they are executed directly. Read-only commands read the
//! database (SQLite WAL allows concurrent readers).

mod output;

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use macsync_daemon::config::{self, Config};
use macsync_daemon::ipc::{self, IpcRequest};
use macsync_daemon::{Daemon, Node};
use macsync_net::client::HelloInfo;
use macsync_net::{pairing, Identity};
use macsync_store::{PeerRow, Store};
use serde_json::{json, Value};

/// Package version plus the Git commit the binary was built from.
const VERSION: &str =
    concat!(env!("CARGO_PKG_VERSION"), " (", env!("MACSYNC_GIT"), ", ", env!("MACSYNC_GIT_DATE"), ")");

#[derive(Parser)]
#[command(name = "macsync", version = VERSION, about = "Git-aware LAN synchronisation between two Macs")]
struct Cli {
    /// Configuration file (default: ~/.config/macsync/config.toml or $MACSYNC_CONFIG).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// More output: log messages; for `dupes` sub-folders (-v) and files (-vv).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the configuration and the device identity.
    Init {
        /// Root directory whose sub-directories are synchronised.
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        name: Option<String>,
        /// Protocol listen address (default 0.0.0.0:47123).
        #[arg(long)]
        listen: Option<SocketAddr>,
        /// Fixed peer address host:port (otherwise Bonjour / pairing address).
        #[arg(long)]
        peer: Option<String>,
        /// two-way | mirror-send | mirror-receive
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Overwrite an existing configuration.
        #[arg(long)]
        force: bool,
    },
    /// Take over configuration and state from RepoSync (≤ 0.2.x); runs
    /// automatically before every command.
    Migrate,
    /// Find Macs that wait for pairing (Bonjour).
    Discover {
        #[arg(long, default_value_t = 4)]
        timeout_s: u64,
        /// One JSON array instead of text lines.
        #[arg(long)]
        json: bool,
    },
    /// Pair with the other Mac (one-time, 6-digit code).
    Pair {
        #[command(subcommand)]
        cmd: PairCmd,
    },
    /// List paired peers.
    Peers,
    /// Revoke a paired peer immediately.
    Unpair { device: String },
    /// Run the daemon in the foreground (used by launchd).
    Daemon,
    /// State, pending changes and conflicts.
    Status {
        unit: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Which paths differ from the peer (and what would happen).
    Diff {
        unit: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// One directory with the state of every item on both Macs.
    Browse {
        /// Directory relative to the sync root, e.g. `notes/sub` (default: root).
        #[arg(default_value = "")]
        dir: String,
        #[arg(long)]
        json: bool,
    },
    /// Synchronise now.
    Sync {
        units: Vec<String>,
        /// Show what would be transferred without writing anything.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Full hash comparison against the file system.
    Verify {
        unit: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List conflicts.
    Conflicts {
        /// Include resolved conflicts.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Resolve conflicts (one or more ids, or --all).
    Resolve {
        #[arg(required_unless_present = "all")]
        ids: Vec<i64>,
        /// All open conflicts (with --unit: of that unit and the units below it).
        #[arg(long, conflicts_with = "ids")]
        all: bool,
        #[arg(long, requires = "all")]
        unit: Option<String>,
        /// local | remote | both
        #[arg(long)]
        keep: String,
    },
    /// Read the journal.
    Log {
        #[arg(long)]
        unit: Option<String>,
        /// e.g. 30m, 1h, 2d
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value_t = 200)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Pause synchronisation (globally or for one unit).
    Pause { unit: Option<String> },
    /// Resume synchronisation.
    Resume { unit: Option<String> },
    /// Direction preference for a unit: local ("this Mac wins"), remote or none.
    Prefer { unit: String, side: String },
    /// Trash with previous versions (copy-on-write, kept 14 days).
    Trash {
        #[command(subcommand)]
        cmd: TrashCmd,
    },
    /// Restore a file from the trash (path as shown by `trash list`).
    Restore {
        path: String,
        /// Destination (default: original location, must not exist).
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// Find same-named folders and files and show which copy is current, or
    /// compare an older copy with a newer one (read-only; needs no
    /// configuration).
    Dupes {
        /// One folder (duplicates inside it) or two folders: the older copy
        /// first, the newer second (what of the older can go?).
        #[arg(required = true, num_args = 1..=2)]
        paths: Vec<PathBuf>,
        /// Two folders: find same-named folders and files across and inside
        /// both instead of comparing the older with the newer.
        #[arg(long)]
        within: bool,
        /// Skip names matching this glob (repeatable), e.g. `--exclude '*.tmp'`.
        #[arg(long)]
        exclude: Vec<String>,
        /// Also look into .git, node_modules, build/cache folders and Mac metadata.
        #[arg(long)]
        no_default_excludes: bool,
        /// Show every pair, file and group (default: the largest ones).
        #[arg(long)]
        all: bool,
        /// ALT NEU: list identical files as well (`state: "same"`).
        #[arg(long, conflicts_with = "within")]
        files: bool,
        /// ALT NEU: carry out a plan (JSON, `-` for stdin): copy, move or
        /// replace files of ALT into NEU, see docs/reconcile.md.
        #[arg(long, value_name = "PLAN", conflicts_with_all = ["within", "files"])]
        apply: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// LaunchAgent / systemd user service.
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },
}

#[derive(Subcommand)]
enum PairCmd {
    /// Show a code and wait for the other Mac.
    Listen {
        #[arg(long, default_value_t = 300)]
        timeout_s: u64,
        /// JSON lines: {"event":"code",…} then {"event":"paired",…}.
        #[arg(long)]
        json: bool,
    },
    /// Pair with a listening Mac.
    Connect {
        /// host or host:port of the listening Mac.
        addr: String,
        code: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TrashCmd {
    List,
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Print the service definition.
    Print,
    /// Install the service definition for the current user.
    Install,
    /// Remove the service definition.
    Uninstall,
}

fn main() {
    let cli = Cli::parse();
    macsync_net::set_build_version(VERSION);
    let default = match (&cli.cmd, cli.verbose) {
        (Cmd::Dupes { .. }, _) => "warn",
        (_, 1..) => "debug",
        (Cmd::Daemon, _) => "info",
        _ => "warn",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("MACSYNC_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

struct DupesOpts {
    within: bool,
    exclude: Vec<String>,
    no_defaults: bool,
    all: bool,
    files: bool,
    apply: Option<PathBuf>,
    json: bool,
    verbose: u8,
}

fn dupes(paths: &[PathBuf], o: DupesOpts) -> Result<()> {
    let DupesOpts { within, exclude, no_defaults, all, files, apply, json, verbose } = o;
    use std::io::IsTerminal;
    if let Some(plan) = apply {
        let [old, new] = paths else { bail!("--apply needs two folders: ALT NEU") };
        return dupes_apply(old, new, &plan, json);
    }
    let mut opts =
        macsync_dupes::Options { progress: !json && std::io::stderr().is_terminal(), files, ..Default::default() };
    if no_defaults {
        opts.exclude.clear();
        opts.skip_caches = false;
    }
    opts.exclude.extend(exclude);
    if let [old, new] = paths {
        if !within {
            let c = macsync_dupes::compare(old, new, &opts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&c)?);
            } else {
                output::compare(&c, verbose, all);
            }
            return Ok(());
        }
    }
    if files {
        bail!("--files needs two folders: ALT NEU");
    }
    let r = macsync_dupes::scan(paths, &opts)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        output::dupes(&r, all);
    }
    Ok(())
}

fn dupes_apply(old: &Path, new: &Path, plan: &Path, json: bool) -> Result<()> {
    let text = if plan == Path::new("-") {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(plan).with_context(|| format!("{}", plan.display()))?
    };
    let plan: macsync_dupes::Plan = serde_json::from_str(&text).with_context(|| format!("{}", plan.display()))?;
    let trash = macsync_dupes::Trash::from_env();
    let s = macsync_dupes::apply(old, new, &plan, &trash, |o| {
        if json {
            println!("{}", serde_json::to_string(o).expect("serializable"));
        } else {
            output::applied(o);
        }
    })?;
    if json {
        println!("{}", serde_json::to_string(&s)?);
    } else {
        output::apply_summary(&s);
    }
    if s.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn config_path(cli: &Cli) -> PathBuf {
    cli.config.clone().unwrap_or_else(config::default_config_path)
}

fn open_store(cfg: &Config) -> Result<Store> {
    Store::open(&macsync_store::db_path(&cfg.state_dir())?)
}

/// Run `req` in the daemon, or `local` if no daemon is running.
fn via_daemon(cfg: &Config, req: IpcRequest, local: impl FnOnce(&Node) -> Result<Value>) -> Result<Value> {
    if let Some(v) = ipc::call(&cfg.state_dir(), &req)? {
        return Ok(v);
    }
    let node = Node::open(cfg.clone())?;
    local(&node)
}

fn run(cli: Cli) -> Result<()> {
    // Only for the standard installation: a test or custom setup passed with
    // --config must never move the real configuration of the user.
    let moved =
        if config_path(&cli) == config::default_config_path() { macsync_daemon::migrate::legacy() } else { Vec::new() };
    if matches!(cli.cmd, Cmd::Migrate) {
        if moved.is_empty() {
            println!("nothing to migrate");
        }
        for m in &moved {
            println!("{m}");
        }
        return Ok(());
    }
    for m in &moved {
        eprintln!("migration: {m}");
    }
    let path = config_path(&cli);
    if let Cmd::Init { root, name, listen, peer, mode, state_dir, force } = &cli.cmd {
        return init(&path, root, name, *listen, peer, mode, state_dir, *force);
    }
    if let Cmd::Discover { timeout_s, json } = cli.cmd {
        return discover(&path, timeout_s, json);
    }
    if let Cmd::Dupes { paths, within, exclude, no_default_excludes, all, files, apply, json } = cli.cmd {
        let o = DupesOpts {
            within,
            exclude,
            no_defaults: no_default_excludes,
            all,
            files,
            apply,
            json,
            verbose: cli.verbose,
        };
        return dupes(&paths, o);
    }
    let cfg = Config::load(&path)?;
    match cli.cmd {
        Cmd::Init { .. } | Cmd::Discover { .. } | Cmd::Dupes { .. } | Cmd::Migrate => unreachable!(),
        Cmd::Pair { cmd } => pair(&cfg, cmd),
        Cmd::Peers => {
            let peers = open_store(&cfg)?.peers()?;
            if peers.is_empty() {
                println!("not paired");
            }
            for p in peers {
                println!("{}  {}  {}  paired {}", p.device, p.name, p.addr, macsync_fs::time::rfc3339(p.paired_ms));
            }
            Ok(())
        }
        Cmd::Unpair { device } => {
            if open_store(&cfg)?.remove_peer(&device)? {
                println!("peer {device} removed; it can no longer connect");
                Ok(())
            } else {
                bail!("no such peer {device}")
            }
        }
        Cmd::Daemon => {
            let node = Node::open(cfg)?;
            let d = Daemon::start(node)?;
            eprintln!("macsync daemon running on {} (Ctrl-C to stop)", d.server_addr());
            d.run_until_signal()
        }
        Cmd::Status { unit, json } => {
            let v = via_daemon(&cfg, IpcRequest::Status, |n| {
                let remote = n.connect_peer().ok().and_then(|p| macsync_engine::PeerApi::roots(&p).ok());
                let map = remote.map(|r| r.into_iter().map(|r| (r.name, r.root)).collect());
                let mut v = n.status(map.as_ref(), &Default::default())?;
                v["daemon"] = json!(false);
                v["peer_online"] = json!(map.is_some());
                Ok(v)
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&filter_units(v, unit.as_deref()))?);
            } else {
                output::status(&filter_units(v, unit.as_deref()));
            }
            Ok(())
        }
        Cmd::Diff { unit, json } => {
            let units = unit.map(|u| vec![u]);
            let v = via_daemon(&cfg, IpcRequest::Sync { units: units.clone(), dry_run: true }, |n| {
                let p = n.connect_peer()?;
                Ok(serde_json::to_value(n.sync_once(&p, units.as_deref(), true)?)?)
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                output::reports(&v, true);
            }
            Ok(())
        }
        Cmd::Browse { dir, json } => {
            let (unit, sub) = split_root_path(&cfg, &dir)?;
            let req = IpcRequest::Browse { unit: unit.clone(), dir: sub.clone() };
            let v = via_daemon(&cfg, req, |n| {
                let p = n.connect_peer().ok();
                let items =
                    n.engine.browse(p.as_ref().map(|p| p as &dyn macsync_engine::PeerApi), unit.as_deref(), &sub)?;
                let name = n.engine.store().peers()?.into_iter().last().map(|r| r.name);
                Ok(json!({ "online": p.is_some(), "peer": name, "items": items }))
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                output::browse(&v);
            }
            Ok(())
        }
        Cmd::Sync { units, dry_run, json } => {
            let units = (!units.is_empty()).then_some(units);
            let v = via_daemon(&cfg, IpcRequest::Sync { units: units.clone(), dry_run }, |n| {
                let p = n.connect_peer()?;
                Ok(serde_json::to_value(n.sync_once(&p, units.as_deref(), dry_run)?)?)
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                output::reports(&v, dry_run);
            }
            Ok(())
        }
        Cmd::Verify { unit, json } => {
            let v = via_daemon(&cfg, IpcRequest::Verify { unit: unit.clone() }, |n| {
                let r = match &unit {
                    Some(u) => vec![n.engine.verify(u)?],
                    None => n.engine.scan_all(true)?,
                };
                Ok(serde_json::to_value(r)?)
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                output::verify(&v);
            }
            Ok(())
        }
        Cmd::Conflicts { all, json } => {
            let rows = open_store(&cfg)?.conflicts(all)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                output::conflicts(&rows);
            }
            Ok(())
        }
        Cmd::Resolve { ids, all, unit, keep } => {
            macsync_engine::Choice::parse(&keep).context("--keep must be local, remote or both")?;
            let ids = if all {
                let prefix = unit.as_ref().map(|u| format!("{u}/"));
                open_store(&cfg)?
                    .conflicts(false)?
                    .into_iter()
                    .filter(|c| match (&unit, &prefix) {
                        (Some(u), Some(p)) => &c.unit == u || c.unit.starts_with(p.as_str()),
                        _ => true,
                    })
                    .map(|c| c.id)
                    .collect()
            } else {
                ids
            };
            if ids.is_empty() {
                println!("no open conflicts");
                return Ok(());
            }
            let req = IpcRequest::ResolveMany { ids: ids.clone(), choice: keep.clone() };
            let v = via_daemon(&cfg, req, |n| ipc::resolve_many(&n.engine, &ids, &keep))?;
            let mut failed = 0;
            for r in v.as_array().into_iter().flatten() {
                let msg = r["message"].as_str().unwrap_or_default();
                if r["ok"].as_bool() == Some(true) {
                    println!("{msg}");
                } else {
                    failed += 1;
                    eprintln!("error: {msg}");
                }
            }
            if failed > 0 {
                bail!("{failed} of {} conflicts not resolved", ids.len());
            }
            Ok(())
        }
        Cmd::Log { unit, since, limit, json } => {
            let since_ms = match since {
                Some(s) => macsync_fs::time::now_ms() - parse_duration(&s)?.as_millis() as i64,
                None => 0,
            };
            let rows = open_store(&cfg)?.journal(since_ms, unit.as_deref(), limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                output::journal(&rows);
            }
            Ok(())
        }
        Cmd::Pause { unit } => {
            let v =
                via_daemon(&cfg, IpcRequest::Pause { unit: unit.clone() }, |n| local_pause(n, unit.as_deref(), true))?;
            println!("{}", v.as_str().unwrap_or_default());
            Ok(())
        }
        Cmd::Resume { unit } => {
            let v = via_daemon(&cfg, IpcRequest::Resume { unit: unit.clone() }, |n| {
                local_pause(n, unit.as_deref(), false)
            })?;
            println!("{}", v.as_str().unwrap_or_default());
            Ok(())
        }
        Cmd::Prefer { unit, side } => {
            let p = macsync_core::Prefer::parse(&side).context("side must be local, remote or none")?;
            if !open_store(&cfg)?.set_prefer(&unit, p)? {
                bail!("unknown unit {unit}");
            }
            println!("{unit}: prefer {}", p.as_str());
            Ok(())
        }
        Cmd::Trash { cmd: TrashCmd::List } => {
            let trash = cfg.root().join(".macsync/trash");
            output::trash(&trash)?;
            Ok(())
        }
        Cmd::Restore { path, to } => restore(&cfg, &path, to.as_deref()),
        Cmd::Service { cmd } => service(&cfg, &path, cmd),
    }
}

fn local_pause(n: &Node, unit: Option<&str>, pause: bool) -> Result<Value> {
    let store = n.engine.store();
    match unit {
        Some(u) => {
            if !store.set_paused(u, pause)? {
                bail!("unknown unit {u}");
            }
        }
        None => store.set_kv("paused_all", if pause { "1" } else { "0" })?,
    }
    Ok(json!(if pause { "paused" } else { "resumed" }))
}

fn filter_units(mut v: Value, unit: Option<&str>) -> Value {
    if let (Some(u), Some(arr)) = (unit, v.get_mut("units").and_then(|a| a.as_array_mut())) {
        arr.retain(|x| x["name"] == u);
    }
    v
}

/// Splits a root-relative directory into (unit, unit-relative dir): the
/// longest known unit name that prefixes the path wins (nested repositories).
fn split_root_path(cfg: &Config, dir: &str) -> Result<(Option<String>, String)> {
    let dir = dir.trim_matches('/');
    if dir.is_empty() || dir == "." {
        return Ok((None, String::new()));
    }
    let mut units: Vec<String> = open_store(cfg)?.units()?.into_iter().map(|u| u.name).collect();
    units.sort_by_key(|u| std::cmp::Reverse(u.len()));
    for u in units {
        if dir == u {
            return Ok((Some(u), String::new()));
        }
        if let Some(rest) = dir.strip_prefix(&format!("{u}/")) {
            return Ok((Some(u), rest.to_string()));
        }
    }
    // Not a unit here (yet): the first component is the unit.
    let (u, rest) = dir.split_once('/').unwrap_or((dir, ""));
    Ok((Some(u.to_string()), rest.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn init(
    path: &Path,
    root: &Path,
    name: &Option<String>,
    listen: Option<SocketAddr>,
    peer: &Option<String>,
    mode: &Option<String>,
    state_dir: &Option<PathBuf>,
    force: bool,
) -> Result<()> {
    if path.exists() && !force {
        bail!("{} already exists (use --force to overwrite)", path.display());
    }
    let mut cfg = Config { root: std::path::absolute(config::expand(root))?, ..Config::default() };
    if let Some(n) = name {
        cfg.device_name = n.clone();
    }
    if let Some(l) = listen {
        cfg.listen = l;
        cfg.pair_port = l.port().wrapping_add(1);
    }
    cfg.peer.address = peer.clone();
    if let Some(m) = mode {
        cfg.mode = m.clone();
    }
    cfg.state_dir = state_dir.clone();
    cfg.validate()?;
    std::fs::create_dir_all(cfg.root())?;
    cfg.save(path)?;
    let id = Identity::load_or_create(&cfg.state_dir())?;
    let _ = Store::open(&macsync_store::db_path(&cfg.state_dir())?)?;
    println!("configuration written to {}", path.display());
    println!("device  {} ({})", cfg.device_name, id.device_id());
    println!("root    {}", cfg.root().display());
    println!("state   {}", cfg.state_dir().display());
    println!("\nnext: `macsync pair listen` on one Mac, `macsync pair connect <host> <code>` on the other");
    Ok(())
}

fn local_ips() -> Vec<String> {
    let mut ips: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.ip())
        .filter(|ip| !ip.is_loopback() && ip.is_ipv4() && macsync_net::lan::is_local(ip))
        .map(|ip| ip.to_string())
        .collect();
    ips.sort();
    ips.dedup();
    ips
}

/// Macs waiting for pairing. Works without configuration (no own id to skip).
fn discover(config_path: &Path, timeout_s: u64, json: bool) -> Result<()> {
    let own = Config::load(config_path).ok().and_then(|c| Identity::load_or_create(&c.state_dir()).ok());
    let own = own.map(|id| id.device_id());
    let found = macsync_net::discovery::browse_pairing(own.as_deref(), Duration::from_secs(timeout_s))?;
    if json {
        println!("{}", serde_json::to_string(&found)?);
    } else {
        for c in found {
            println!("{}\t{}\t{}", c.addr, c.device, c.name);
        }
    }
    Ok(())
}

/// Store the paired peer; returns a JSON description incl. warnings.
fn store_peer(cfg: &Config, r: &pairing::PairResult) -> Result<Value> {
    let store = open_store(cfg)?;
    let mut warnings = Vec::new();
    for p in store.peers()? {
        if p.device != r.info.device {
            store.remove_peer(&p.device)?;
            warnings.push(format!("replacing previous peer {} ({})", p.name, p.device));
        }
    }
    let addr = SocketAddr::new(r.addr.ip(), r.port);
    store.add_peer(&PeerRow {
        device: r.info.device.clone(),
        name: r.info.name.clone(),
        pubkey: r.pubkey.clone(),
        addr: addr.to_string(),
        paired_ms: macsync_fs::time::now_ms(),
    })?;
    if r.info.arch != std::env::consts::ARCH {
        warnings.push(format!(
            "the other Mac is {} and this one {}: native modules in node_modules (.node, esbuild, sharp) may not work on both",
            r.info.arch,
            std::env::consts::ARCH
        ));
    }
    if r.info.root != cfg.root().to_string_lossy() {
        warnings.push(format!(
            "root path differs ({} vs {}); absolute paths in virtualenvs and caches will point to the wrong place",
            r.info.root,
            cfg.root().display()
        ));
    }
    Ok(json!({
        "event": "paired",
        "device": r.info.device,
        "name": r.info.name,
        "addr": addr.to_string(),
        "warnings": warnings,
    }))
}

fn print_paired(v: &Value, json: bool) {
    if json {
        println!("{v}");
        return;
    }
    for w in v["warnings"].as_array().into_iter().flatten() {
        println!("warning: {}", w.as_str().unwrap_or_default());
    }
    println!(
        "paired with {} ({}) at {}",
        v["name"].as_str().unwrap_or("?"),
        v["device"].as_str().unwrap_or("?"),
        v["addr"].as_str().unwrap_or("?")
    );
}

fn pair(cfg: &Config, cmd: PairCmd) -> Result<()> {
    let id = Identity::load_or_create(&cfg.state_dir())?;
    let hello = HelloInfo::local(&id, &cfg.device_name, &cfg.root().to_string_lossy());
    let port = cfg.listen.port();
    match cmd {
        PairCmd::Listen { timeout_s, json } => {
            let listener = TcpListener::bind(SocketAddr::new(cfg.listen.ip(), cfg.pair_port))
                .with_context(|| format!("bind pairing port {}", cfg.pair_port))?;
            let code = pairing::generate_code();
            // Lets the other Mac find this one without knowing its name.
            let _advert =
                macsync_net::discovery::Advertiser::start_pairing(&id.device_id(), &cfg.device_name, cfg.pair_port)
                    .map_err(|e| tracing::warn!("Bonjour announcement failed: {e:#}"))
                    .ok();
            let ips = local_ips();
            if json {
                println!(
                    "{}",
                    json!({"event": "code", "code": code, "ips": ips, "port": cfg.pair_port, "timeout_s": timeout_s})
                );
            } else {
                println!("pairing code: {} {}", &code[..3], &code[3..]);
                if ips.is_empty() {
                    println!("on the other Mac run: macsync pair connect <this-mac>.local {code}");
                }
                for ip in ips {
                    println!("on the other Mac run: macsync pair connect {ip}:{} {code}", cfg.pair_port);
                }
                println!("waiting up to {timeout_s} s …");
            }
            let r = pairing::listen(&listener, &code, &id, &hello, port, Duration::from_secs(timeout_s))?;
            print_paired(&store_peer(cfg, &r)?, json);
            Ok(())
        }
        PairCmd::Connect { addr, code, json } => {
            let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
            let with_port = if addr.contains(':') && !addr.starts_with('[') || addr.contains("]:") {
                addr.clone()
            } else {
                format!("{addr}:{}", cfg.pair_port)
            };
            let sa = macsync_net::lan::resolve(&with_port)?;
            let r = pairing::connect(sa, &code, &id, &hello, port)?;
            print_paired(&store_peer(cfg, &r)?, json);
            Ok(())
        }
    }
}

fn restore(cfg: &Config, rel: &str, to: Option<&Path>) -> Result<()> {
    macsync_core::path::validate(rel).map_err(anyhow::Error::msg)?;
    let trash = cfg.root().join(".macsync/trash");
    let src = trash.join(rel);
    if std::fs::symlink_metadata(&src).is_err() {
        bail!("{} not found in the trash", rel);
    }
    // Layout: <tx>/<root-relative path>
    let original = rel.split_once('/').map(|(_, r)| r).context("expected <transaction>/<path>")?;
    let dest = match to {
        Some(t) => t.to_path_buf(),
        None => cfg.root().join(original),
    };
    if std::fs::symlink_metadata(&dest).is_ok() {
        bail!("{} exists; use --to to restore elsewhere", dest.display());
    }
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    macsync_fs::ops::copy_tree(&src, &dest)?;
    println!("restored {}", dest.display());
    Ok(())
}

fn service(cfg: &Config, config_path: &Path, cmd: ServiceCmd) -> Result<()> {
    use macsync_daemon::service as svc;
    let bin = std::env::current_exe()?;
    let def = if cfg!(target_os = "macos") {
        svc::launchd_plist(&bin, config_path, &cfg.state_dir())
    } else {
        svc::systemd_unit(&bin, config_path)
    };
    let target = svc::install_path();
    match cmd {
        ServiceCmd::Print => print!("{def}"),
        ServiceCmd::Install => {
            if let Some(p) = target.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::write(&target, def)?;
            println!("written {}", target.display());
            if cfg!(target_os = "macos") {
                println!("activate with: launchctl bootstrap gui/$(id -u) {}", target.display());
            } else {
                println!("activate with: systemctl --user daemon-reload && systemctl --user enable --now macsync");
            }
        }
        ServiceCmd::Uninstall => {
            if cfg!(target_os = "macos") {
                println!("first run: launchctl bootout gui/$(id -u)/{}", svc::LABEL);
            }
            let _ = std::fs::remove_file(&target);
            println!("removed {}", target.display());
        }
    }
    Ok(())
}

fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num.parse().with_context(|| format!("invalid duration {s:?}"))?;
    let secs = match unit {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => bail!("invalid duration unit in {s:?} (use s, m, h, d)"),
    };
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90").unwrap().as_secs(), 90);
        assert_eq!(parse_duration("30m").unwrap().as_secs(), 1800);
        assert_eq!(parse_duration("1h").unwrap().as_secs(), 3600);
        assert_eq!(parse_duration("2d").unwrap().as_secs(), 172_800);
        assert!(parse_duration("1w").is_err());
    }

    #[test]
    fn cli_parses() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
