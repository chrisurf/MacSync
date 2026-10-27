//! Human readable output.

use std::path::Path;

use macsync_store::{ConflictRow, LogRow};
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
        "MacSync · {} ({}) · {} · root {}{}",
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
    println!("version  {}", s(&v["version"]));
    if let Some(pv) = v.get("peer_version").filter(|p| !p.is_null()) {
        let note =
            if pv == &v["version"] { " (same build)" } else { "  ⚠ different build: run `make update` on both Macs" };
        println!("         peer {}{note}", s(pv));
    }
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
            macsync_fs::time::rfc3339(c.created_ms)
        );
        for r in &c.remotes {
            println!("       remote: {} ({}){}", r.path, r.vv.display(), if r.deleted { " deleted" } else { "" });
        }
        println!("       stored: {}", c.stored);
    }
    println!("\nresolve with: macsync resolve <id>... --keep local|remote|both");
    println!("          or: macsync resolve --all [--unit <unit>] --keep local|remote");
}

pub fn journal(rows: &[LogRow]) {
    for r in rows {
        println!(
            "{}  {:<10} {:<24} {:<40} {}{}",
            macsync_fs::time::rfc3339(r.ts_ms),
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

/// `macsync browse`: one line per item.
pub fn browse(v: &Value) {
    let peer = v["peer"].as_str().unwrap_or("peer");
    if v["online"] != Value::Bool(true) {
        println!("({peer} not reachable – local view only)");
    }
    for i in v["items"].as_array().into_iter().flatten() {
        let state = match i["state"].as_str().unwrap_or("") {
            "synced" => "in sync".to_string(),
            "to-peer" => format!("→ {peer}"),
            "from-peer" => format!("← {peer}"),
            "delete-on-peer" => format!("delete on {peer}"),
            "delete-here" => "delete here".to_string(),
            "differs" => "changes inside".to_string(),
            "unknown" => "?".to_string(),
            s => s.to_string(),
        };
        let dir = if i["kind"] == "dir" { "/" } else { "" };
        let paused = if i["paused"] == Value::Bool(true) { " (paused)" } else { "" };
        println!("{:<22} {}{dir}{paused}", state, i["name"].as_str().unwrap_or(""));
    }
}

/// `macsync dupes`: folder pairs, then same-named files.
pub fn dupes(r: &macsync_dupes::Report, all: bool) {
    use macsync_dupes::{GroupKind, Verdict};
    let t = macsync_fs::time::local;
    let limit = |n: usize| if all { usize::MAX } else { n };
    let roots: Vec<String> = r.roots.iter().map(|p| p.display().to_string()).collect();
    println!(
        "{}: {} files, {} folders, {} · {} compared by content · {:.1} s",
        roots.join(" ↔ "),
        r.files,
        r.dirs,
        bytes(r.bytes),
        r.hashed_files,
        r.elapsed_ms as f64 / 1000.0
    );
    if r.dataless > 0 {
        println!("{} iCloud files not downloaded: compared by size and date only", r.dataless);
    }
    for n in &r.notes {
        println!("note: {n}");
    }
    if !r.skipped.is_empty() {
        println!("{} paths not readable (see --json)", r.skipped.len());
    }

    println!("\nFOLDERS ({})", r.pairs.len());
    if r.pairs.is_empty() {
        println!("  no same-named folders with the same content");
    }
    for (k, p) in r.pairs.iter().take(limit(25)).enumerate() {
        let (a, b) = (&p.a.display, &p.b.display);
        println!();
        match p.verdict {
            Verdict::Identical => {
                println!("{:>2}. IDENTICAL  {a}", k + 1);
                println!("               {b}");
                println!("    → same {} files: one copy can go (frees {})", p.identical, bytes(p.reclaimable_bytes));
            }
            Verdict::Newer => {
                println!("{:>2}. NEWER      {a}   ← current", k + 1);
                println!("    OLDER      {b}");
                println!(
                    "    → the current copy contains everything of the older one: it can go (frees {})",
                    bytes(p.reclaimable_bytes)
                );
            }
            Verdict::Diverged => {
                println!("{:>2}. DIVERGED   {a}   ← changed more recently", k + 1);
                println!("               {b}");
                println!("    → both contain something the other lacks: merge first");
            }
        }
        let mut facts = vec![format!("{} identical", p.identical + p.unverified)];
        for (n, what) in [
            (p.newer_a.len(), format!("newer in {a}")),
            (p.newer_b.len(), format!("newer in {b}")),
            (p.only_a.len(), format!("only in {a}")),
            (p.only_b.len(), format!("only in {b}")),
            (p.unclear.len(), "different, same date".to_string()),
        ] {
            if n > 0 {
                facts.push(format!("{n} {what}"));
            }
        }
        println!("    {}", facts.join(" · "));
        // The copy event explains the history: when the copy was made, and
        // which side was worked on afterwards.
        for s in [&p.a, &p.b] {
            if let Some(e) = s.copied {
                println!(
                    "    {}: {} of {} files created {} – copied or moved here",
                    s.display,
                    e.files,
                    s.files,
                    t(e.at_ms)
                );
            }
        }
        if let (Some(x), Some(y)) = (p.a.changed_since_copy, p.b.changed_since_copy) {
            println!("    changed after that: {x} in {a}, {y} in {b}");
        }
        println!("    last change: {} in {a}, {} in {b}", t(p.a.newest_ms), t(p.b.newest_ms));
        let max = limit(8);
        // Dates in the order of the title: the newer side first.
        let changed = |title: String, l: &[macsync_dupes::Changed], b_first: bool| {
            if l.is_empty() {
                return;
            }
            println!("    {title}:");
            for c in l.iter().take(max) {
                let (x, y) = if b_first { (c.mtime_b, c.mtime_a) } else { (c.mtime_a, c.mtime_b) };
                println!("      {}  ({} vs {})", c.path, t(x), t(y));
            }
            if l.len() > max {
                println!("      … {} more (--all)", l.len() - max);
            }
        };
        let only = |title: String, l: &[macsync_dupes::Item]| {
            if l.is_empty() {
                return;
            }
            println!("    {title}:");
            for i in l.iter().take(max) {
                println!("      {}  ({}, {})", i.path, bytes(i.size), t(i.mtime_ms));
            }
            if l.len() > max {
                println!("      … {} more (--all)", l.len() - max);
            }
        };
        changed(format!("newer in {a} (vs {b})"), &p.newer_a, false);
        changed(format!("newer in {b} (vs {a})"), &p.newer_b, true);
        changed(format!("different, same date ({a} vs {b})"), &p.unclear, false);
        only(format!("only in {a}"), &p.only_a);
        only(format!("only in {b}"), &p.only_b);
    }
    if r.pairs.len() > limit(25) {
        println!("\n… {} more folder pairs (--all)", r.pairs.len() - 25);
    }

    let dup = r.groups.iter().filter(|g| g.kind == GroupKind::Duplicates).count();
    println!("\nFILES ({dup} with identical copies, {} with different versions)", r.groups.len() - dup);
    if r.groups.is_empty() {
        println!("  no other same-named files");
    }
    for g in r.groups.iter().take(limit(30)) {
        println!();
        match g.kind {
            GroupKind::Duplicates => {
                println!("{}  {} identical copies (frees {})", g.name, g.members.len(), bytes(g.reclaimable_bytes))
            }
            GroupKind::Versions => {
                let n = g.members.iter().map(|m| m.content).max().unwrap_or(0);
                println!("{}  {} copies, {n} versions", g.name, g.members.len())
            }
        }
        for m in &g.members {
            let mut tags = Vec::new();
            if g.kind == GroupKind::Versions {
                tags.push(format!("v{}", m.content));
            }
            if m.newest {
                tags.push("newest".into());
            }
            if m.paired {
                tags.push("in folder pair".into());
            }
            if m.dataless {
                tags.push("iCloud".into());
            }
            println!("  {:<60} {:>9}  {}  {}", m.display, bytes(m.size), t(m.mtime_ms), tags.join(", "));
        }
    }
    if r.groups.len() > limit(30) {
        println!("\n… {} more file groups (--all)", r.groups.len() - 30);
    }
    println!("\nCan be freed without losing anything: {}", bytes(r.reclaimable_bytes));
    println!("Nothing was changed. Check before deleting – and prefer the Trash.");
}

/// `macsync dupes ALT NEU`: what of the older copy can go.
pub fn compare(c: &macsync_dupes::Comparison, verbose: u8, all: bool) {
    use macsync_dupes::{FolderStatus as S, OrderStatus};
    let t = macsync_fs::time::local;
    println!(
        "ALT  {}  ({} files, {}, last change {})",
        c.old.display(),
        c.old_files,
        bytes(c.old_bytes),
        t(c.order.old_last_ms)
    );
    println!(
        "NEU  {}  ({} files, {}, last change {})",
        c.new.display(),
        c.new_files,
        bytes(c.new_bytes),
        t(c.order.new_last_ms)
    );
    let o = &c.order;
    let changed = o.newer_in_new + o.newer_in_old;
    match o.status {
        OrderStatus::Confirmed => {
            println!("✅ Order confirmed: NEU is newer ({} of {changed} changed files are newer there)", o.newer_in_new)
        }
        OrderStatus::Contradicted => {
            println!("⚠️  Order doubtful: {} of {changed} changed files are newer in ALT.", o.newer_in_old);
            println!("    Is ALT the current copy? The report still treats the first folder as ALT.");
        }
        OrderStatus::Unknown if changed == 0 => println!("ℹ️  Order not checkable: no file differs between the two"),
        OrderStatus::Unknown => {
            println!("⚠️  Order unclear: {} changed files are newer in NEU, {} in ALT", o.newer_in_new, o.newer_in_old)
        }
    }
    if c.dataless > 0 {
        println!("☁️  {} iCloud files not downloaded: compared by size and date only", c.dataless);
    }
    if !c.skipped.is_empty() {
        println!("❌ {} paths not readable{}", c.skipped.len(), if verbose == 0 { " (-v lists them)" } else { ":" });
        if verbose > 0 {
            for s in c.skipped.iter().take(if all { usize::MAX } else { 10 }) {
                println!("     {}  ({})", s.path.display(), s.reason);
            }
        }
    }
    println!();

    let root = &c.folders[0];
    let tops: Vec<usize> = root.children.iter().copied().filter(|&i| c.folders[i].in_old).collect();
    let ok = tops.iter().filter(|&&i| c.folders[i].status.deletable()).count();
    match root.status {
        S::Identical => println!("✅ ALT is identical to NEU – it can go completely ({})", bytes(root.old_bytes)),
        S::Deletable => {
            println!("🚮 ALT can go completely – NEU contains all of it, partly newer ({})", bytes(root.old_bytes))
        }
        _ => {
            println!("⚠️  ALT cannot go completely – {ok} of {} folders can ({})", tops.len(), bytes(c.deletable_bytes))
        }
    }
    println!();
    let mut rows = root.children.clone();
    rows.sort_by_key(|&i| (rank(c.folders[i].status), c.folders[i].name.to_lowercase()));
    let width = rows.iter().map(|&i| c.folders[i].name.chars().count()).max().unwrap_or(0).min(32);
    for &i in &rows {
        row(c, i, 0, width);
        if verbose > 0 && matches!(c.folders[i].status, S::Check | S::Unreadable) {
            drill(c, i, 1, verbose, all);
        } else if verbose > 1 && c.folders[i].status == S::OnlyOld {
            files(c, i, 1, all);
        }
    }
    if root.own.old_files() + root.own.only_new > 0 {
        println!("📄 {:<width$}  {}", "(files directly in ALT)", counts(&root.own));
        if verbose > 1 {
            files(c, 0, 1, all);
        }
    }

    println!();
    if !root.status.deletable() && c.deletable_bytes > 0 {
        println!("Can go from ALT without losing anything: {}", bytes(c.deletable_bytes));
    }
    let check: Vec<&str> = rows
        .iter()
        .filter(|&&i| matches!(c.folders[i].status, S::Check | S::OnlyOld | S::Unreadable))
        .map(|&i| c.folders[i].name.as_str())
        .collect();
    if !check.is_empty() {
        println!("Check first: {}", check.join(", "));
        if verbose == 0 {
            println!("Details: -v (sub-folders), -vv (files)");
        }
    }
    println!(
        "\nFiles: ✅ identical · 🔼 newer in NEU · 🔀 moved in NEU · ❗ newer in ALT · ❓ different, same date · \
         🟥 only in ALT · ➕ only in NEU · ❌ not readable\n\
         Folders: ✅ identical · 🚮 can go · 🟧 check · 🟥 only in ALT · ➕ only in NEU · ❌ not readable"
    );
    println!("Nothing was changed. Check before deleting – and prefer the Trash.");
}

fn rank(s: macsync_dupes::FolderStatus) -> u8 {
    use macsync_dupes::FolderStatus as S;
    match s {
        S::Unreadable => 0,
        S::Check => 1,
        S::OnlyOld => 2,
        S::Deletable => 3,
        S::Identical => 4,
        S::OnlyNew => 5,
    }
}

/// One line per folder: symbol, name, file states, size in ALT.
fn row(c: &macsync_dupes::Comparison, i: usize, depth: usize, width: usize) {
    use macsync_dupes::FolderStatus as S;
    let f = &c.folders[i];
    let n = &f.total;
    let (sym, what, size) = match f.status {
        S::Identical => ("✅", format!("identical · {} files", n.same), f.old_bytes),
        S::Deletable => ("🚮", format!("all in NEU · {}", counts(n)), f.old_bytes),
        S::Check => ("🟧", counts(n), f.old_bytes),
        S::OnlyOld => ("🟥", format!("only in ALT · {} files", n.old_files()), f.old_bytes),
        S::OnlyNew => ("➕", format!("only in NEU · {} files", n.only_new), f.new_bytes),
        S::Unreadable => ("❌", format!("not fully readable · {}", counts(n)), f.old_bytes),
    };
    let indent = "   ".repeat(depth);
    println!("{indent}{sym} {:<width$}  {what}  ({})", f.name, bytes(size));
}

/// Below a folder that cannot go: the sub-folders that cannot go either, the
/// rest in one line each.
fn drill(c: &macsync_dupes::Comparison, i: usize, depth: usize, verbose: u8, all: bool) {
    use macsync_dupes::FolderStatus as S;
    let f = &c.folders[i];
    let indent = "   ".repeat(depth);
    let mut bad: Vec<usize> = f
        .children
        .iter()
        .copied()
        .filter(|&k| matches!(c.folders[k].status, S::Check | S::OnlyOld | S::Unreadable))
        .collect();
    bad.sort_by_key(|&k| (rank(c.folders[k].status), c.folders[k].name.to_lowercase()));
    let width = bad.iter().map(|&k| c.folders[k].name.chars().count()).max().unwrap_or(0).min(32);
    for &k in &bad {
        row(c, k, depth, width);
        match c.folders[k].status {
            S::OnlyOld if verbose > 1 => files(c, k, depth + 1, all),
            S::OnlyOld => {}
            _ => drill(c, k, depth + 1, verbose, all),
        }
    }
    let ok: Vec<usize> = f.children.iter().copied().filter(|&k| c.folders[k].status.deletable()).collect();
    if !ok.is_empty() {
        let b: u64 = ok.iter().map(|&k| c.folders[k].old_bytes).sum();
        println!("{indent}🚮 {} can go ({})", plural(ok.len() as u64, "more sub-folder"), bytes(b));
    }
    let new = f.children.iter().filter(|&&k| c.folders[k].status == S::OnlyNew).count();
    if new > 0 {
        println!("{indent}➕ {} only in NEU", plural(new as u64, "sub-folder"));
    }
    let own = &f.own;
    if own.risky() > 0 {
        if verbose > 1 {
            files(c, i, depth, all);
        } else {
            let risky = macsync_dupes::Counts { same: 0, updated: 0, moved: 0, only_new: 0, ..*own };
            println!("{indent}📄 {} directly here: {}", plural(own.risky(), "file"), counts(&risky));
        }
    }
}

/// The files of ALT below folder `i` that would be lost.
fn files(c: &macsync_dupes::Comparison, i: usize, depth: usize, all: bool) {
    use macsync_dupes::State;
    let t = macsync_fs::time::local;
    let indent = "   ".repeat(depth);
    let base = &c.folders[i].path;
    // An only-in-ALT folder lists everything below it, others their own files.
    let mut list = Vec::new();
    let mut stack = vec![i];
    while let Some(d) = stack.pop() {
        list.extend(c.folders[d].entries.iter().map(|&e| &c.entries[e]).filter(|e| e.state.risky()));
        if c.folders[d].status == macsync_dupes::FolderStatus::OnlyOld {
            stack.extend(&c.folders[d].children);
        }
    }
    list.sort_by(|a, b| a.path.cmp(&b.path));
    let max = if all { usize::MAX } else { 10 };
    for e in list.iter().take(max) {
        let rel = e.path.strip_prefix(base.as_str()).map_or(e.path.as_str(), |r| r.trim_start_matches('/'));
        let (sym, what) = match e.state {
            State::OldNewer => {
                ("❗", format!("ALT {} · NEU {}", t(e.mtime_old.unwrap_or(0)), t(e.mtime_new.unwrap_or(0))))
            }
            State::Unclear => ("❓", format!("different content, same date {}", t(e.mtime_old.unwrap_or(0)))),
            State::Unreadable => ("❌", "not readable in NEU".to_string()),
            _ => ("🟥", format!("{}, {}", bytes(e.size), t(e.mtime_old.unwrap_or(0)))),
        };
        println!("{indent}{sym} {rel}  ({what})");
    }
    if list.len() > max {
        println!("{indent}… {} more (--all)", list.len() - max);
    }
}

fn plural(n: u64, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

fn counts(n: &macsync_dupes::Counts) -> String {
    let parts = [
        ("✅", n.same),
        ("🔼", n.updated),
        ("🔀", n.moved),
        ("❗", n.old_newer),
        ("❓", n.unclear),
        ("🟥", n.only_old),
        ("➕", n.only_new),
        ("❌", n.unreadable),
    ];
    let v: Vec<String> = parts.iter().filter(|(_, k)| *k > 0).map(|(s, k)| format!("{s} {k}")).collect();
    if v.is_empty() {
        "empty".into()
    } else {
        v.join("  ")
    }
}

/// One finished action of `dupes --apply`.
/// One progress line on the terminal, overwritten by the next.
pub fn apply_progress(p: &macsync_dupes::Progress, total: u64) {
    eprint!("\r\x1b[K{} of {} (all: {}) · {}", bytes(p.bytes_done), bytes(p.bytes), bytes(total), p.file);
}

pub fn applied(o: &macsync_dupes::Outcome, tty: bool) {
    use macsync_dupes::Done;
    if tty {
        eprint!("\r\x1b[K");
    }
    let target = if o.to == o.path { String::new() } else { format!(" → {}", o.to) };
    match o.result {
        Done::Done => println!("✅ {}{target}", o.path),
        Done::Skipped => println!("⏭️  {}{target}: {}", o.path, o.error.as_deref().unwrap_or("skipped")),
        Done::Failed => println!("❌ {}{target}: {}", o.path, o.error.as_deref().unwrap_or("failed")),
        Done::Cancelled => println!("⏹  {}{target}: cancelled", o.path),
    }
    if let Some(t) = &o.trashed {
        println!("   in the Trash: {t}");
    }
}

pub fn apply_summary(s: &macsync_dupes::Summary) {
    println!();
    println!("{} done ({}), {} skipped, {} failed", s.done, bytes(s.bytes), s.skipped, s.failed);
    if s.cancelled {
        println!("Cancelled: what is done stays, the rest was not started; NEU is otherwise unchanged.");
    }
}

fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}
