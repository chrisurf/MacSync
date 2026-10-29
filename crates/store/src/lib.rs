//! SQLite persistence (WAL mode).
//!
//! Tables: `units`, `entries` (index + stat cache + version vectors +
//! tombstones), `tx` (transaction journal for crash recovery), `journal`
//! (human readable action log), `conflicts`, `peers`, `kv`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use macsync_core::{Entry, Hash, Kind, Prefer, UnitKind, VersionVector};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS units (
    name TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    root BLOB,
    paused INTEGER NOT NULL DEFAULT 0,
    prefer TEXT NOT NULL DEFAULT 'none',
    synced_ms INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS entries (
    unit TEXT NOT NULL,
    path TEXT NOT NULL,
    disk TEXT NOT NULL,
    kind INTEGER NOT NULL,
    mode INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    blob BLOB,
    cmp BLOB,
    target BLOB,
    xattrs BLOB,
    flags INTEGER NOT NULL DEFAULT 0,
    link TEXT,
    deleted INTEGER NOT NULL,
    vv TEXT NOT NULL,
    ino INTEGER NOT NULL DEFAULT 0,
    dev INTEGER NOT NULL DEFAULT 0,
    ctime_ns INTEGER NOT NULL DEFAULT 0,
    changed_ms INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (unit, path)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS entries_blob ON entries(blob) WHERE deleted = 0;
CREATE TABLE IF NOT EXISTS tx (
    id TEXT PRIMARY KEY,
    unit TEXT NOT NULL,
    state TEXT NOT NULL,
    plan BLOB NOT NULL,
    created_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS journal (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms INTEGER NOT NULL,
    unit TEXT NOT NULL,
    action TEXT NOT NULL,
    path TEXT NOT NULL,
    reason TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS journal_ts ON journal(ts_ms);
CREATE TABLE IF NOT EXISTS conflicts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    unit TEXT NOT NULL,
    path TEXT NOT NULL,
    class TEXT NOT NULL,
    local BLOB,
    remote BLOB NOT NULL,
    remote_state BLOB NOT NULL,
    stored TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    resolved_ms INTEGER,
    resolution TEXT
);
CREATE INDEX IF NOT EXISTS conflicts_open ON conflicts(unit, path) WHERE resolved_ms IS NULL;
CREATE TABLE IF NOT EXISTS peers (
    device TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    pubkey BLOB NOT NULL,
    addr TEXT NOT NULL DEFAULT '',
    paired_ms INTEGER NOT NULL
);
"#;

/// Locally observed stat data, used to skip hashing unchanged files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatCache {
    pub ino: u64,
    pub dev: u64,
    pub ctime_ns: i64,
}

/// An entry as stored locally.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    pub entry: Entry,
    /// Unit-relative path as spelled on the local disk.
    pub disk: String,
    pub stat: StatCache,
    /// When this row last changed state (tombstone age).
    pub changed_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitRow {
    pub name: String,
    pub kind: UnitKind,
    pub root: Option<Hash>,
    pub paused: bool,
    pub prefer: Prefer,
    pub synced_ms: i64,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogRow {
    pub id: i64,
    pub ts_ms: i64,
    pub unit: String,
    pub action: String,
    pub path: String,
    pub reason: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConflictRow {
    pub id: i64,
    pub unit: String,
    pub path: String,
    pub class: String,
    /// Local entries at the time of the conflict (several for group conflicts).
    pub locals: Vec<Entry>,
    /// Remote entries that were held back.
    pub remotes: Vec<Entry>,
    /// Location of the remote version(s) in the conflict store.
    pub stored: String,
    pub created_ms: i64,
    pub resolved_ms: Option<i64>,
    pub resolution: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerRow {
    pub device: String,
    pub name: String,
    pub pubkey: Vec<u8>,
    pub addr: String,
    pub paired_ms: i64,
}

#[derive(Clone, Debug)]
pub struct TxRow {
    pub id: String,
    pub unit: String,
    pub state: String,
    pub plan: Vec<u8>,
    pub created_ms: i64,
}

pub struct Store {
    conn: Connection,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn hash_col(h: &Option<Hash>) -> Option<Vec<u8>> {
    h.map(|h| h.0.to_vec())
}

fn read_hash(v: Option<Vec<u8>>) -> Option<Hash> {
    v.and_then(|b| Hash::from_slice(&b))
}

const ENTRY_COLS: &str =
    "path, disk, kind, mode, size, mtime_ns, blob, cmp, target, xattrs, flags, link, deleted, vv, ino, dev, ctime_ns, changed_ms";

fn row_to_stored(r: &Row) -> rusqlite::Result<Stored> {
    let kind: i64 = r.get(2)?;
    let xattrs: Option<Vec<u8>> = r.get(9)?;
    let vv: String = r.get(13)?;
    Ok(Stored {
        entry: Entry {
            path: r.get(0)?,
            kind: Kind::from_i64(kind).unwrap_or(Kind::File),
            mode: r.get::<_, i64>(3)? as u32,
            size: r.get::<_, i64>(4)? as u64,
            mtime_ns: r.get(5)?,
            blob: read_hash(r.get(6)?),
            cmp: read_hash(r.get(7)?),
            target: r.get(8)?,
            xattrs: xattrs.and_then(|b| macsync_core::codec::decode(&b).ok()).unwrap_or_default(),
            flags: r.get::<_, i64>(10)? as u32,
            link: r.get(11)?,
            deleted: r.get::<_, i64>(12)? != 0,
            vv: serde_json::from_str(&vv).unwrap_or_default(),
        },
        disk: r.get(1)?,
        stat: StatCache { ino: r.get::<_, i64>(14)? as u64, dev: r.get::<_, i64>(15)? as u64, ctime_ns: r.get(16)? },
        changed_ms: r.get(17)?,
    })
}

/// File name of the database inside the state directory.
pub const DB_FILE: &str = "macsync.db";
/// Name up to 0.2.x (RepoSync), renamed on first use.
const LEGACY_DB_FILE: &str = "reposync.db";

/// Path of the database in `state_dir`; takes over a database from the
/// RepoSync era (`reposync.db` incl. WAL files) if there is no new one yet.
pub fn db_path(state_dir: &Path) -> Result<PathBuf> {
    let db = state_dir.join(DB_FILE);
    let legacy = state_dir.join(LEGACY_DB_FILE);
    if !db.exists() && legacy.exists() {
        for suffix in ["", "-wal", "-shm"] {
            let from = state_dir.join(format!("{LEGACY_DB_FILE}{suffix}"));
            if from.exists() {
                std::fs::rename(&from, state_dir.join(format!("{DB_FILE}{suffix}")))
                    .with_context(|| format!("rename {}", from.display()))?;
            }
        }
    }
    Ok(db)
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let conn = Connection::open(path).with_context(|| format!("open database {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        let s = Store { conn };
        match s.kv("schema_version")? {
            None => s.set_kv("schema_version", &SCHEMA_VERSION.to_string())?,
            Some(v) if v == SCHEMA_VERSION.to_string() => {}
            Some(v) => anyhow::bail!("unsupported database schema version {v}"),
        }
        Ok(s)
    }

    /// Run `f` inside one SQLite transaction.
    pub fn atomically<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let tx = self.conn.unchecked_transaction()?;
        let out = f(self)?;
        tx.commit()?;
        Ok(out)
    }

    // ---- kv -------------------------------------------------------------

    pub fn kv(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_kv(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM kv WHERE key = ?1", [key])?;
        Ok(())
    }

    // ---- units ------------------------------------------------------------

    fn row_to_unit(r: &Row) -> rusqlite::Result<UnitRow> {
        let kind: String = r.get(1)?;
        let prefer: String = r.get(4)?;
        Ok(UnitRow {
            name: r.get(0)?,
            kind: UnitKind::parse(&kind).unwrap_or(UnitKind::Folder),
            root: read_hash(r.get(2)?),
            paused: r.get::<_, i64>(3)? != 0,
            prefer: Prefer::parse(&prefer).unwrap_or_default(),
            synced_ms: r.get(5)?,
            status: r.get(6)?,
        })
    }

    pub fn units(&self) -> Result<Vec<UnitRow>> {
        let mut st =
            self.conn.prepare("SELECT name, kind, root, paused, prefer, synced_ms, status FROM units ORDER BY name")?;
        let rows = st.query_map([], Self::row_to_unit)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn unit(&self, name: &str) -> Result<Option<UnitRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT name, kind, root, paused, prefer, synced_ms, status FROM units WHERE name = ?1",
                [name],
                Self::row_to_unit,
            )
            .optional()?)
    }

    /// Create the unit or update its kind (Folder ↔ Git keeps all entries).
    pub fn upsert_unit(&self, name: &str, kind: UnitKind) -> Result<()> {
        self.conn.execute(
            "INSERT INTO units(name, kind) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET kind = excluded.kind",
            params![name, kind.as_str()],
        )?;
        Ok(())
    }

    pub fn set_unit_root(&self, name: &str, root: Hash) -> Result<()> {
        self.conn.execute("UPDATE units SET root = ?2 WHERE name = ?1", params![name, root.0.to_vec()])?;
        Ok(())
    }

    pub fn set_unit_status(&self, name: &str, status: &str, synced: bool) -> Result<()> {
        if synced {
            self.conn.execute(
                "UPDATE units SET status = ?2, synced_ms = ?3 WHERE name = ?1",
                params![name, status, now_ms()],
            )?;
        } else {
            self.conn.execute("UPDATE units SET status = ?2 WHERE name = ?1", params![name, status])?;
        }
        Ok(())
    }

    pub fn set_paused(&self, name: &str, paused: bool) -> Result<bool> {
        Ok(self.conn.execute("UPDATE units SET paused = ?2 WHERE name = ?1", params![name, paused as i64])? > 0)
    }

    pub fn set_prefer(&self, name: &str, prefer: Prefer) -> Result<bool> {
        Ok(self.conn.execute("UPDATE units SET prefer = ?2 WHERE name = ?1", params![name, prefer.as_str()])? > 0)
    }

    pub fn delete_unit(&self, name: &str) -> Result<()> {
        self.conn.execute("DELETE FROM entries WHERE unit = ?1", [name])?;
        self.conn.execute("DELETE FROM units WHERE name = ?1", [name])?;
        Ok(())
    }

    /// Rename a unit and every unit nested below it (`old/…` → `new/…`),
    /// keeping entries, version vectors and open conflicts. Used when only the
    /// spelling of the directory changed (case or normalisation).
    pub fn rename_unit(&self, old: &str, new: &str) -> Result<()> {
        self.atomically(|st| {
            for (table, col) in [("units", "name"), ("entries", "unit"), ("conflicts", "unit")] {
                st.conn.execute(
                    &format!(
                        "UPDATE {table} SET {col} = ?2 || substr({col}, length(?1) + 1)
                         WHERE {col} = ?1 OR substr({col}, 1, length(?1) + 1) = ?1 || '/'"
                    ),
                    params![old, new],
                )?;
            }
            Ok(())
        })
    }

    /// Drop a unit row that only duplicates another unit under a different
    /// spelling. Its entries are forgotten without tombstones (the files
    /// belong to the other unit) and its open conflicts are closed.
    pub fn retire_unit(&self, name: &str) -> Result<usize> {
        self.atomically(|st| {
            let closed = st.conn.execute(
                "UPDATE conflicts SET resolved_ms = ?2, resolution = 'superseded' WHERE unit = ?1 AND resolved_ms IS NULL",
                params![name, now_ms()],
            )?;
            st.delete_unit(name)?;
            Ok(closed)
        })
    }

    // ---- entries ------------------------------------------------------------

    pub fn entries(&self, unit: &str) -> Result<Vec<Stored>> {
        let mut st = self.conn.prepare_cached(&format!("SELECT {ENTRY_COLS} FROM entries WHERE unit = ?1"))?;
        let rows = st.query_map([unit], row_to_stored)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Entries that exist (no tombstones). After `cargo clean` a unit can hold
    /// hundreds of thousands of tombstones that only matter path by path.
    pub fn live_entries(&self, unit: &str) -> Result<Vec<Stored>> {
        let mut st =
            self.conn.prepare_cached(&format!("SELECT {ENTRY_COLS} FROM entries WHERE unit = ?1 AND deleted = 0"))?;
        let rows = st.query_map([unit], row_to_stored)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Up to `limit` live entries in path order, starting after `after`
    /// (`None`: from the start, including the unit's own entry `""`).
    /// Pages through a large unit without holding the store for long.
    pub fn live_entries_after(&self, unit: &str, after: Option<&str>, limit: usize) -> Result<Vec<Stored>> {
        let mut st = self.conn.prepare_cached(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE unit = ?1 AND (?2 IS NULL OR path > ?2) AND deleted = 0
             ORDER BY path LIMIT ?3"
        ))?;
        let rows =
            st.query_map(params![unit, after, limit as i64], row_to_stored)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn entry(&self, unit: &str, path: &str) -> Result<Option<Stored>> {
        let mut st =
            self.conn.prepare_cached(&format!("SELECT {ENTRY_COLS} FROM entries WHERE unit = ?1 AND path = ?2"))?;
        Ok(st.query_row(params![unit, path], row_to_stored).optional()?)
    }

    /// All entries equal to or below `prefix` (the empty prefix selects all).
    pub fn entries_under(&self, unit: &str, prefix: &str) -> Result<Vec<Stored>> {
        if prefix.is_empty() {
            return self.entries(unit);
        }
        let mut st = self.conn.prepare_cached(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE unit = ?1 AND (path = ?2 OR (path >= ?3 AND path < ?4))"
        ))?;
        // '/' + 1 == '0': selects exactly the paths starting with "prefix/".
        let lo = format!("{prefix}/");
        let hi = format!("{prefix}0");
        let rows = st.query_map(params![unit, prefix, lo, hi], row_to_stored)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn put_entry(&self, unit: &str, s: &Stored) -> Result<()> {
        let e = &s.entry;
        let xattrs = if e.xattrs.is_empty() { None } else { Some(macsync_core::codec::encode(&e.xattrs)?) };
        let mut st = self.conn.prepare_cached(&format!(
            "INSERT OR REPLACE INTO entries(unit, {ENTRY_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)"
        ))?;
        st.execute(params![
            unit,
            e.path,
            s.disk,
            e.kind.as_i64(),
            e.mode as i64,
            e.size as i64,
            e.mtime_ns,
            hash_col(&e.blob),
            hash_col(&e.cmp),
            e.target,
            xattrs,
            e.flags as i64,
            e.link,
            e.deleted as i64,
            serde_json::to_string(&e.vv)?,
            s.stat.ino as i64,
            s.stat.dev as i64,
            s.stat.ctime_ns,
            s.changed_ms,
        ])?;
        Ok(())
    }

    pub fn remove_entry(&self, unit: &str, path: &str) -> Result<()> {
        self.conn.execute("DELETE FROM entries WHERE unit = ?1 AND path = ?2", params![unit, path])?;
        Ok(())
    }

    /// Live local copies of a blob anywhere in the root (for deduplication and
    /// for serving). Returns `(unit, path, disk path)`.
    pub fn find_blob(&self, blob: &Hash) -> Result<Vec<(String, String, String)>> {
        let mut st = self.conn.prepare_cached(
            "SELECT unit, path, disk FROM entries WHERE blob = ?1 AND deleted = 0 AND kind = 0 LIMIT 8",
        )?;
        let rows = st
            .query_map([blob.0.to_vec()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Remove tombstones older than `cutoff_ms` in `unit`.
    pub fn purge_tombstones(&self, unit: &str, cutoff_ms: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM entries WHERE unit = ?1 AND deleted = 1 AND changed_ms < ?2",
            params![unit, cutoff_ms],
        )?)
    }

    pub fn entry_count(&self, unit: &str) -> Result<(u64, u64)> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(deleted = 0), 0), COALESCE(SUM(deleted = 1), 0) FROM entries WHERE unit = ?1",
            [unit],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
        )?)
    }

    // ---- transaction journal -------------------------------------------------

    pub fn tx_put(&self, id: &str, unit: &str, state: &str, plan: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO tx(id, unit, state, plan, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, unit, state, plan, now_ms()],
        )?;
        Ok(())
    }

    pub fn tx_set_state(&self, id: &str, state: &str) -> Result<()> {
        self.conn.execute("UPDATE tx SET state = ?2 WHERE id = ?1", params![id, state])?;
        Ok(())
    }

    pub fn tx_remove(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM tx WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn tx_pending(&self) -> Result<Vec<TxRow>> {
        let mut st = self.conn.prepare("SELECT id, unit, state, plan, created_ms FROM tx ORDER BY created_ms")?;
        let rows = st
            .query_map([], |r| {
                Ok(TxRow { id: r.get(0)?, unit: r.get(1)?, state: r.get(2)?, plan: r.get(3)?, created_ms: r.get(4)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- journal ----------------------------------------------------------

    pub fn log(&self, unit: &str, action: &str, path: &str, reason: &str, detail: &str) -> Result<()> {
        let mut st = self.conn.prepare_cached(
            "INSERT INTO journal(ts_ms, unit, action, path, reason, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        st.execute(params![now_ms(), unit, action, path, reason, detail])?;
        Ok(())
    }

    /// The latest warning about `.git/config` or a hook (not Git's `*.sample`
    /// files) of `unit` changed by sync: `(path, ts_ms)`.
    pub fn git_config_warning(&self, unit: &str) -> Result<Option<(String, i64)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT path, ts_ms FROM journal WHERE unit = ?1 AND action = 'warning'
                 AND (path = '.git/config' OR (path LIKE '.git/hooks/%' AND path NOT LIKE '%.sample'))
                 ORDER BY id DESC LIMIT 1",
                params![unit],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn journal(&self, since_ms: i64, unit: Option<&str>, limit: usize) -> Result<Vec<LogRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, ts_ms, unit, action, path, reason, detail FROM journal
             WHERE ts_ms >= ?1 AND (?2 IS NULL OR unit = ?2) ORDER BY id DESC LIMIT ?3",
        )?;
        let mut rows = st
            .query_map(params![since_ms, unit, limit as i64], |r| {
                Ok(LogRow {
                    id: r.get(0)?,
                    ts_ms: r.get(1)?,
                    unit: r.get(2)?,
                    action: r.get(3)?,
                    path: r.get(4)?,
                    reason: r.get(5)?,
                    detail: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }

    pub fn prune_journal(&self, before_ms: i64) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM journal WHERE ts_ms < ?1", [before_ms])?)
    }

    // ---- conflicts ----------------------------------------------------------

    /// Record a conflict unless an identical open one (same path, same remote
    /// state) exists. Returns the id of the new or existing conflict and
    /// whether it is new. Group conflicts (Git state) carry several entries.
    pub fn add_conflict(
        &self,
        unit: &str,
        path: &str,
        class: &str,
        locals: &[Entry],
        remotes: &[Entry],
        stored: &str,
    ) -> Result<(i64, bool)> {
        let state = remote_state(remotes);
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM conflicts WHERE unit = ?1 AND path = ?2 AND remote_state = ?3 AND resolved_ms IS NULL",
                params![unit, path, state.0.to_vec()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok((id, false));
        }
        // A newer remote version supersedes older open conflicts on the same path.
        self.conn.execute(
            "UPDATE conflicts SET resolved_ms = ?3, resolution = 'superseded' WHERE unit = ?1 AND path = ?2 AND resolved_ms IS NULL",
            params![unit, path, now_ms()],
        )?;
        self.conn.execute(
            "INSERT INTO conflicts(unit, path, class, local, remote, remote_state, stored, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                unit,
                path,
                class,
                macsync_core::codec::encode(locals)?,
                macsync_core::codec::encode(remotes)?,
                state.0.to_vec(),
                stored,
                now_ms()
            ],
        )?;
        Ok((self.conn.last_insert_rowid(), true))
    }

    /// True if an open conflict with exactly this remote state exists.
    pub fn has_open_conflict(&self, unit: &str, path: &str, remotes: &[Entry]) -> Result<bool> {
        let state = remote_state(remotes);
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM conflicts WHERE unit = ?1 AND path = ?2 AND remote_state = ?3 AND resolved_ms IS NULL",
                params![unit, path, state.0.to_vec()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    fn row_to_conflict(r: &Row) -> rusqlite::Result<ConflictRow> {
        let local: Option<Vec<u8>> = r.get(4)?;
        let remote: Vec<u8> = r.get(5)?;
        let decode = |b: &[u8]| {
            macsync_core::codec::decode::<Vec<Entry>>(b)
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Blob, Box::new(e)))
        };
        Ok(ConflictRow {
            id: r.get(0)?,
            unit: r.get(1)?,
            path: r.get(2)?,
            class: r.get(3)?,
            locals: local.as_deref().map(decode).transpose()?.unwrap_or_default(),
            remotes: decode(&remote)?,
            stored: r.get(6)?,
            created_ms: r.get(7)?,
            resolved_ms: r.get(8)?,
            resolution: r.get(9)?,
        })
    }

    pub fn conflicts(&self, include_resolved: bool) -> Result<Vec<ConflictRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, unit, path, class, local, remote, stored, created_ms, resolved_ms, resolution FROM conflicts
             WHERE (?1 OR resolved_ms IS NULL) ORDER BY id",
        )?;
        let rows = st.query_map([include_resolved], Self::row_to_conflict)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn conflict(&self, id: i64) -> Result<Option<ConflictRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, unit, path, class, local, remote, stored, created_ms, resolved_ms, resolution FROM conflicts WHERE id = ?1",
                [id],
                Self::row_to_conflict,
            )
            .optional()?)
    }

    pub fn open_conflict_for(&self, unit: &str, path: &str) -> Result<Option<ConflictRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, unit, path, class, local, remote, stored, created_ms, resolved_ms, resolution FROM conflicts
                 WHERE unit = ?1 AND path = ?2 AND resolved_ms IS NULL",
                params![unit, path],
                Self::row_to_conflict,
            )
            .optional()?)
    }

    pub fn resolve_conflict(&self, id: i64, resolution: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE conflicts SET resolved_ms = ?2, resolution = ?3 WHERE id = ?1",
            params![id, now_ms(), resolution],
        )?;
        Ok(())
    }

    /// Close open conflicts on `path` because a newer version superseded them.
    pub fn supersede_conflicts(&self, unit: &str, path: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE conflicts SET resolved_ms = ?3, resolution = 'superseded' WHERE unit = ?1 AND path = ?2 AND resolved_ms IS NULL",
            params![unit, path, now_ms()],
        )?)
    }

    pub fn open_conflict_count(&self, unit: &str) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM conflicts WHERE unit = ?1 AND resolved_ms IS NULL",
            [unit],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    // ---- peers ------------------------------------------------------------

    pub fn add_peer(&self, p: &PeerRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO peers(device, name, pubkey, addr, paired_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![p.device, p.name, p.pubkey, p.addr, p.paired_ms],
        )?;
        Ok(())
    }

    pub fn peers(&self) -> Result<Vec<PeerRow>> {
        let mut st = self.conn.prepare("SELECT device, name, pubkey, addr, paired_ms FROM peers ORDER BY paired_ms")?;
        let rows = st
            .query_map([], |r| {
                Ok(PeerRow {
                    device: r.get(0)?,
                    name: r.get(1)?,
                    pubkey: r.get(2)?,
                    addr: r.get(3)?,
                    paired_ms: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_peer_addr(&self, device: &str, addr: &str) -> Result<()> {
        self.conn.execute("UPDATE peers SET addr = ?2 WHERE device = ?1", params![device, addr])?;
        Ok(())
    }

    pub fn remove_peer(&self, device: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM peers WHERE device = ?1", [device])? > 0)
    }
}

/// Identity of a set of remote entries (for conflict de-duplication).
pub fn remote_state(remotes: &[Entry]) -> Hash {
    let mut h = Vec::with_capacity(remotes.len() * 64);
    for e in remotes {
        h.extend_from_slice(e.path.as_bytes());
        h.push(0);
        h.extend_from_slice(&e.state_hash().0);
    }
    Hash::of(&h)
}

/// Convenience: a fresh version vector for a newly observed local entry.
pub fn first_version(device: &str) -> VersionVector {
    VersionVector::single(device, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(path: &str, content: &[u8]) -> Stored {
        let h = Hash::of(content);
        Stored {
            entry: Entry {
                path: path.into(),
                kind: Kind::File,
                mode: 0o644,
                size: content.len() as u64,
                mtime_ns: 5,
                blob: Some(h),
                cmp: Some(h),
                target: None,
                xattrs: vec![("user.a".into(), vec![1, 2])],
                flags: 0,
                link: None,
                deleted: false,
                vv: VersionVector::single("A", 3),
            },
            disk: path.into(),
            stat: StatCache { ino: 7, dev: 1, ctime_ns: 9 },
            changed_ms: 1,
        }
    }

    #[test]
    fn entries_roundtrip_and_prefix() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_unit("u", UnitKind::Git).unwrap();
        for p in ["a", "a/b", "a/b/c", "ab", "z"] {
            s.put_entry("u", &stored(p, p.as_bytes())).unwrap();
        }
        let e = s.entry("u", "a/b").unwrap().unwrap();
        assert_eq!(e, stored("a/b", b"a/b"));
        let mut under: Vec<_> = s.entries_under("u", "a").unwrap().into_iter().map(|s| s.entry.path).collect();
        under.sort();
        assert_eq!(under, vec!["a", "a/b", "a/b/c"]);
        assert_eq!(s.entries_under("u", "").unwrap().len(), 5);
        assert_eq!(s.find_blob(&Hash::of(b"z")).unwrap(), vec![("u".to_string(), "z".to_string(), "z".to_string())]);
        assert_eq!(s.entry_count("u").unwrap(), (5, 0));
    }

    #[test]
    fn conflicts_dedupe_and_supersede() {
        let s = Store::open_in_memory().unwrap();
        let r1 = stored("f", b"1").entry;
        let r2 = stored("f", b"2").entry;
        let (id1, new1) = s.add_conflict("u", "f", "concurrent-modify", &[], std::slice::from_ref(&r1), "x").unwrap();
        let (id1b, new1b) = s.add_conflict("u", "f", "concurrent-modify", &[], std::slice::from_ref(&r1), "x").unwrap();
        assert!(new1 && !new1b && id1 == id1b);
        assert!(s.has_open_conflict("u", "f", std::slice::from_ref(&r1)).unwrap());
        let (id2, _) = s.add_conflict("u", "f", "concurrent-modify", &[], std::slice::from_ref(&r2), "y").unwrap();
        let open = s.conflicts(false).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id2);
        assert_eq!(s.conflict(id1).unwrap().unwrap().resolution.as_deref(), Some("superseded"));
    }

    #[test]
    fn rename_and_retire_units() {
        let s = Store::open_in_memory().unwrap();
        for u in ["obsidian", "obsidian/verification", "obsidianx"] {
            s.upsert_unit(u, UnitKind::Folder).unwrap();
            s.put_entry(u, &stored("a.md", b"a")).unwrap();
        }
        let e = stored("a.md", b"a").entry;
        s.add_conflict("obsidian/verification", "a.md", "case-collision", &[], &[e], "/c").unwrap();
        s.rename_unit("obsidian", "Obsidian").unwrap();
        let names: Vec<_> = s.units().unwrap().into_iter().map(|u| u.name).collect();
        assert_eq!(names, vec!["Obsidian", "Obsidian/verification", "obsidianx"]);
        assert_eq!(s.entries("Obsidian/verification").unwrap().len(), 1);
        assert_eq!(s.open_conflict_count("Obsidian/verification").unwrap(), 1);
        assert_eq!(s.retire_unit("Obsidian/verification").unwrap(), 1);
        assert!(s.unit("Obsidian/verification").unwrap().is_none());
        assert!(s.entries("Obsidian/verification").unwrap().is_empty());
        assert_eq!(s.entries("obsidianx").unwrap().len(), 1, "prefix match is per component");
        s.set_kv("k", "v").unwrap();
        s.delete_kv("k").unwrap();
        assert!(s.kv("k").unwrap().is_none());
    }

    #[test]
    fn units_and_kv() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_unit("p", UnitKind::Folder).unwrap();
        s.upsert_unit("p", UnitKind::Git).unwrap();
        assert!(s.set_paused("p", true).unwrap());
        assert!(s.set_prefer("p", Prefer::Local).unwrap());
        let u = s.unit("p").unwrap().unwrap();
        assert_eq!((u.kind, u.paused, u.prefer), (UnitKind::Git, true, Prefer::Local));
        s.set_kv("k", "v").unwrap();
        assert_eq!(s.kv("k").unwrap().as_deref(), Some("v"));
        s.log("p", "apply", "x", "test", "").unwrap();
        assert_eq!(s.journal(0, Some("p"), 10).unwrap().len(), 1);
    }

    #[test]
    fn atomically_rolls_back_on_error() {
        let s = Store::open_in_memory().unwrap();
        let r: Result<()> = s.atomically(|s| {
            s.set_kv("k", "v")?;
            anyhow::bail!("boom")
        });
        assert!(r.is_err());
        assert_eq!(s.kv("k").unwrap(), None);
    }
}
