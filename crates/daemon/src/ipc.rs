//! Local control socket (`<state_dir>/daemon.sock`), one JSON request per
//! line. Used by the CLI and the future menu bar app. Only the owning user can
//! connect (socket mode 0600).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use macsync_core::Prefer;
use macsync_engine::Choice;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::runtime::{Daemon, Node, Shared};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum IpcRequest {
    Status,
    Sync {
        units: Option<Vec<String>>,
        dry_run: bool,
    },
    Pause {
        unit: Option<String>,
    },
    Resume {
        unit: Option<String>,
    },
    Prefer {
        unit: String,
        side: String,
    },
    Resolve {
        id: i64,
        choice: String,
    },
    Verify {
        unit: Option<String>,
    },
    Rescan,
    /// Remember the current network, so the daemon listens here even with
    /// `hide_in_unknown_networks`.
    TrustNetwork,
    /// Synchronise the unit(s) behind `path` next (relative to the root).
    Prioritize {
        path: String,
    },
    /// One directory with the state of every item on both Macs. `path`
    /// (relative to the sync root) replaces `unit`/`dir`. `local`: only this
    /// Mac's state, never the network (fast, for the first paint).
    Browse {
        #[serde(default)]
        unit: Option<String>,
        #[serde(default)]
        dir: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        local: bool,
    },
    /// Conflicts with both versions side by side (the app's conflict view).
    Conflicts {
        #[serde(default)]
        all: bool,
    },
    /// Resolve several conflicts with the same choice.
    ResolveMany {
        ids: Vec<i64>,
        choice: String,
    },
    /// Overwrite: `here` = this Mac's state of `path` replaces the other
    /// Mac's; otherwise the other Mac's state replaces this one.
    Win {
        path: String,
        here: bool,
    },
    /// Hide the notice about a changed `.git/config` or hook of `unit`.
    DismissGitNotice {
        unit: String,
    },
    /// Telemetry events of both Macs since `since_ms` (`macsync diag`).
    Diag {
        since_ms: i64,
        #[serde(default = "yes")]
        peer: bool,
    },
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub error: Option<String>,
}

pub fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join("daemon.sock")
}

/// Send a request to a running daemon. Returns `Ok(None)` if no daemon runs.
pub fn call(state_dir: &Path, req: &IpcRequest) -> Result<Option<Value>> {
    let path = socket_path(state_dir);
    let Ok(mut s) = UnixStream::connect(&path) else { return Ok(None) };
    s.set_read_timeout(Some(Duration::from_secs(3600)))?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    s.write_all(line.as_bytes())?;
    let mut resp = String::new();
    BufReader::new(s).read_line(&mut resp)?;
    let r: IpcResponse = serde_json::from_str(&resp).context("invalid daemon response")?;
    if r.ok {
        Ok(Some(r.data))
    } else {
        bail!(r.error.unwrap_or_else(|| "daemon error".into()))
    }
}

pub(crate) fn serve(path: &Path, node: Arc<Node>, shared: Arc<Shared>) -> Result<()> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            bail!("another daemon is already running ({})", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let busy = Arc::new(AtomicUsize::new(0));
    std::thread::Builder::new().name("macsync-ipc".into()).spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
            // Clients give up after a few seconds and ask again; without a
            // limit a slow request lets the waiting ones pile up without end.
            if busy.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
                busy.fetch_sub(1, Ordering::SeqCst);
                let _ = refuse_busy(&stream);
                macsync_engine::telemetry::event("ipc-busy", json!({"limit": MAX_IN_FLIGHT}));
                continue;
            }
            let (n, s, b) = (node.clone(), shared.clone(), busy.clone());
            let spawned = std::thread::Builder::new().name("macsync-ipc-conn".into()).spawn(move || {
                let _ = handle_conn(stream, &n, &s);
                b.fetch_sub(1, Ordering::SeqCst);
            });
            if spawned.is_err() {
                busy.fetch_sub(1, Ordering::SeqCst);
            }
        }
    })?;
    Ok(())
}

/// App and CLI requests taking longer are recorded (telemetry).
const SLOW_IPC: Duration = Duration::from_millis(500);

/// Requests handled at the same time; more are refused at once.
const MAX_IN_FLIGHT: usize = 16;

fn refuse_busy(mut stream: &UnixStream) -> Result<()> {
    let resp = IpcResponse { ok: false, data: Value::Null, error: Some("daemon busy, try again".into()) };
    let mut out = serde_json::to_string(&resp)?;
    out.push('\n');
    stream.write_all(out.as_bytes())?;
    Ok(())
}

fn handle_conn(stream: UnixStream, node: &Arc<Node>, shared: &Arc<Shared>) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let resp = match serde_json::from_str::<IpcRequest>(&line) {
        Ok(req) => {
            let cmd = serde_json::to_value(&req).ok().and_then(|v| v["cmd"].as_str().map(str::to_string));
            let t = std::time::Instant::now();
            let r = dispatch(req, node, shared);
            let took = t.elapsed();
            if took >= SLOW_IPC {
                macsync_engine::telemetry::event(
                    "ipc-slow",
                    json!({"cmd": cmd, "ms": macsync_engine::telemetry::ms(took),
                           "error": r.as_ref().err().map(|e| format!("{e:#}"))}),
                );
            }
            match r {
                Ok(data) => IpcResponse { ok: true, data, error: None },
                Err(e) => IpcResponse { ok: false, data: Value::Null, error: Some(format!("{e:#}")) },
            }
        }
        Err(e) => IpcResponse { ok: false, data: Value::Null, error: Some(format!("bad request: {e}")) },
    };
    let mut out = serde_json::to_string(&resp)?;
    out.push('\n');
    (&stream).write_all(out.as_bytes())?;
    Ok(())
}

fn dispatch(req: IpcRequest, node: &Arc<Node>, shared: &Arc<Shared>) -> Result<Value> {
    let engine = &node.engine;
    let pause = matches!(req, IpcRequest::Pause { .. });
    Ok(match req {
        IpcRequest::Status => Daemon::status_json(node, shared)?,
        IpcRequest::Sync { units, dry_run } => Daemon::ipc_sync(node, shared, units, dry_run)?,
        IpcRequest::Pause { unit: None } => {
            Daemon::set_paused(node, shared, true)?;
            json!("paused")
        }
        IpcRequest::Resume { unit: None } => {
            Daemon::set_paused(node, shared, false)?;
            json!("resumed")
        }
        IpcRequest::Pause { unit: Some(u) } | IpcRequest::Resume { unit: Some(u) } => {
            if !engine.store().set_paused(&u, pause)? {
                bail!("unknown unit {u}");
            }
            Daemon::kick(shared);
            json!(if pause { "paused" } else { "resumed" })
        }
        IpcRequest::Prefer { unit, side } => {
            let p = Prefer::parse(&side).with_context(|| format!("invalid side {side}"))?;
            if !engine.store().set_prefer(&unit, p)? {
                bail!("unknown unit {unit}");
            }
            json!(p.as_str())
        }
        IpcRequest::Resolve { id, choice } => {
            let c = Choice::parse(&choice).with_context(|| format!("invalid choice {choice}"))?;
            let msg = engine.resolve(id, c)?;
            Daemon::kick(shared);
            json!(msg)
        }
        IpcRequest::Verify { unit } => {
            let r = match unit {
                Some(u) => vec![engine.verify(&u)?],
                None => engine.scan_all(true)?,
            };
            Daemon::kick(shared);
            serde_json::to_value(r)?
        }
        IpcRequest::Rescan => {
            let r = engine.scan_all(false)?;
            Daemon::kick(shared);
            serde_json::to_value(r)?
        }
        IpcRequest::TrustNetwork => Daemon::trust_network(node, shared)?,
        IpcRequest::Prioritize { path } => Daemon::ipc_prioritize(node, shared, &path)?,
        IpcRequest::Browse { unit, dir, path, local } => {
            let (unit, dir) = match path {
                Some(p) => engine.split_path(&p)?,
                None => (unit, dir),
            };
            Daemon::ipc_browse(node, shared, unit.as_deref(), &dir, local)?
        }
        IpcRequest::Conflicts { all } => {
            let name = engine.store().peers()?.into_iter().last().map(|r| r.name);
            json!({ "peer": name, "conflicts": engine.conflict_views(all)? })
        }
        IpcRequest::Diag { since_ms, peer } => Daemon::ipc_diag(node, shared, since_ms, peer)?,
        IpcRequest::Win { path, here } => Daemon::ipc_win(node, shared, &path, here)?,
        IpcRequest::DismissGitNotice { unit } => {
            engine.store().dismiss_git_warning(&unit)?;
            json!("dismissed")
        }
        IpcRequest::ResolveMany { ids, choice } => {
            let v = resolve_many(engine, &ids, &choice)?;
            Daemon::kick(shared);
            v
        }
    })
}

/// `[{id, ok, message}]` for each conflict, in the given order.
pub fn resolve_many(engine: &macsync_engine::Engine, ids: &[i64], choice: &str) -> Result<Value> {
    let c = Choice::parse(choice).with_context(|| format!("invalid choice {choice}"))?;
    let rows: Vec<Value> = engine
        .resolve_many(ids, c)?
        .into_iter()
        .map(|(id, r)| match r {
            Ok(m) => json!({ "id": id, "ok": true, "message": m }),
            Err(e) => json!({ "id": id, "ok": false, "message": format!("{e:#}") }),
        })
        .collect();
    Ok(Value::Array(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_format() {
        let r = IpcRequest::Sync { units: Some(vec!["a".into()]), dry_run: true };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"cmd":"sync","units":["a"],"dry_run":true}"#);
        assert_eq!(serde_json::from_str::<IpcRequest>(r#"{"cmd":"status"}"#).unwrap(), IpcRequest::Status);
    }
}
