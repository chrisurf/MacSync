//! Human readable output.

use std::path::Path;

use reposync_store::{ConflictRow, LogRow};
use serde_json::Value;

fn s(v: &Value) -> String {
    match v {
        Value::Null => "-".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub fn status(v: &Value) {
    println!(
        "RepoSync · {} ({}) · {} · root {}{}",
        s(&v["name"]),
        s(&v["device"]),
        s(&v["mode"]),
        s(&v["root"]),
        if v["paused"] == true { " · PAUSED" } else { "" }
    );
    match v.get("peer").filter(|p| !p.is_null()) {
        Some(p) => {
            let online = if v["peer_online"] == true { "online" } else { "offline" };
            println!("peer     {} ({}) at {} · {online}", s(&p["name"]), s(&p["device"]), s(&p["address"]));
            if let Some(e) = v.get("peer_error").filter(|e| !e.is_null()) {
                println!("         last error: {}", s(e));
            }
        }
        None => println!("peer     not paired"),
    }
    println!("daemon   {}", if v["daemon"] == true { "running" } else { "not running" });
    println!();
    println!(
        "{:<32} {:<6} {:<18} {:<12} {:<12} {:>5} {:>8}  LAST SYNC",
        "UNIT", "KIND", "STATE", "LOCAL", "REMOTE", "CONFL", "ENTRIES"
    );
    for u in v["units"].as_array().into_iter().flatten() {
        println!(
            "{:<32} {:<6} {:<18} {:<12} {:<12} {:>5} {:>8}  {}",
            s(&u["name"]),
            s(&u["kind"]),
            s(&u["state"]),
            s(&u["root"]),
            s(&u["remote_root"]),
            s(&u["conflicts"]),
            s(&u["entries"]),
            s(&u["last_sync"])
        );
    }
}

pub fn reports(v: &Value, dry_run: bool) {
    let mut any = false;
    for r in v.as_array().into_iter().flatten() {
        let outcome = match &r["outcome"] {
            Value::String(o) => o.clone(),
            Value::Object(m) => m.iter().map(|(k, v)| format!("{k}: {}", s(v))).collect::<Vec<_>>().join(", "),
            o => s(o),
        };
        if outcome == "InSync" {
            continue;
        }
        any = true;
        println!(
            "{}: {} · writes {} · deletes {} · conflicts {} · fetched {} B · deduplicated {} B{}",
            s(&r["unit"]),
            outcome,
            s(&r["writes"]),
            s(&r["deletes"]),
            s(&r["conflicts"]),
            s(&r["bytes_fetched"]),
            s(&r["bytes_deduped"]),
            if r["converged"] == true { " · in sync" } else { "" }
        );
        if dry_run {
            for a in r["actions"].as_array().into_iter().flatten() {
                println!("    {:<10} {}  ({})", s(&a["kind"]).to_lowercase(), s(&a["path"]), s(&a["reason"]));
            }
        }
    }
    if !any {
        println!("everything in sync");
    }
}

pub fn verify(v: &Value) {
    for r in v.as_array().into_iter().flatten() {
        let deferred = r.get("deferred").filter(|d| !d.is_null());
        println!(
            "{:<32} root {}  hashed {}  changed {}  silent changes {}{}",
            s(&r["unit"]),
            s(&r["root"]).chars().take(12).collect::<String>(),
            s(&r["hashed"]),
            s(&r["changed"]),
            s(&r["silent_changes"]),
            deferred.map(|d| format!("  (deferred: {})", s(d))).unwrap_or_default()
        );
    }
}

pub fn conflicts(rows: &[ConflictRow]) {
    if rows.is_empty() {
        println!("no conflicts");
        return;
    }
    for c in rows {
        let state = match (&c.resolution, c.resolved_ms) {
            (Some(r), Some(_)) => format!("resolved: {r}"),
            _ => "open".into(),
        };
        println!(
            "#{:<4} {:<18} {}/{}  [{}]  {}",
            c.id,
            c.class,
            c.unit,
            c.path,
            state,
            reposync_fs::time::rfc3339(c.created_ms)
        );
        for r in &c.remotes {
            println!("       remote: {} ({}){}", r.path, r.vv.display(), if r.deleted { " deleted" } else { "" });
        }
        println!("       stored: {}", c.stored);
    }
    println!("\nresolve with: reposync resolve <id> --keep local|remote|both");
}

pub fn journal(rows: &[LogRow]) {
    for r in rows {
        println!(
            "{}  {:<10} {:<24} {:<40} {}{}",
            reposync_fs::time::rfc3339(r.ts_ms),
            r.action,
            r.unit,
            r.path,
            r.reason,
            if r.detail.is_empty() { String::new() } else { format!(" [{}]", r.detail) }
        );
    }
}

pub fn trash(dir: &Path) -> anyhow::Result<()> {
    let mut txs: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => Vec::new(),
    };
    txs.sort_by_key(|e| e.file_name());
    if txs.is_empty() {
        println!("trash is empty");
    }
    for tx in txs {
        let mut files = Vec::new();
        collect(&tx.path(), &tx.path(), &mut files);
        for f in files {
            println!("{}/{}", tx.file_name().to_string_lossy(), f);
        }
    }
    Ok(())
}

fn collect(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            collect(base, &p, out);
        } else {
            out.push(p.strip_prefix(base).unwrap_or(&p).to_string_lossy().into_owned());
        }
    }
}
