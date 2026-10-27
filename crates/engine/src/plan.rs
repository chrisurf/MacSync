//! The transaction plan: an ordered, serialisable list of file operations
//! plus the database rows to write. The plan is persisted in the `tx` table
//! before the first file is touched so that an interrupted apply can be
//! completed after a crash.

use macsync_core::{gitpath, path as rpath, Entry, GitClass, Hash, UnitKind};
use serde::{Deserialize, Serialize};

/// What happens to a path (for reports, dry runs and `diff`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionKind {
    /// New or changed content is written.
    Write,
    /// Only metadata (mode, xattrs, flags, mtime) changes.
    Meta,
    Delete,
    /// Reflogs merged line-wise.
    Merge,
    /// Only the version vector is updated.
    Vv,
    /// The local version is newer; nothing to do here (peer pulls).
    KeepLocal,
    Conflict,
    /// Not applied (nested unit, unsafe object deletion, …).
    Skip,
    /// Only the spelling of the name changes (case-insensitive volume).
    Rename,
}

impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Write => "write",
            ActionKind::Meta => "meta",
            ActionKind::Delete => "delete",
            ActionKind::Merge => "merge",
            ActionKind::Vv => "vv",
            ActionKind::KeepLocal => "keep-local",
            ActionKind::Conflict => "conflict",
            ActionKind::Skip => "skip",
            ActionKind::Rename => "rename",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannedAction {
    pub path: String,
    pub kind: ActionKind,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum OpKind {
    Mkdir,
    Write,
    Symlink,
    Meta,
    Delete,
    DbOnly,
    /// Change the spelling of an existing name in place (`Source::Rename`).
    Rename,
}

/// Execution phase (lower first).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum Phase {
    Dirs = 0,
    Writes = 1,
    Deletes = 2,
    Index = 3,
    ObjectDeletes = 4,
    Db = 5,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Op {
    pub path: String,
    /// Unit-relative target path on the local disk.
    pub disk: String,
    pub kind: OpKind,
    pub phase: Phase,
    /// Git apply rank (worktree for folder units).
    pub rank: u8,
    /// Final entry stored in the database (version vector included).
    pub entry: Entry,
    /// File name inside the transaction's staging directory.
    pub staged: Option<String>,
    /// Content source for staging (not needed after staging).
    pub source: Option<Source>,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Source {
    /// Fetch the blob (locally deduplicated if possible).
    Blob { hash: Hash, size: u64 },
    /// Merge the local reflog at `local_disk` with the remote blob.
    Reflog { local_disk: String, local_blob: Hash, remote: Hash, remote_size: u64 },
    /// Copy from a file in the conflict store (conflict resolution).
    File { path: String, hash: Hash },
    /// Unit-relative disk path of the same file under its previous spelling.
    Rename { from: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ConflictPlan {
    pub path: String,
    pub class: String,
    pub locals: Vec<Entry>,
    pub remotes: Vec<Entry>,
    /// Conflict store directory (absolute) receiving the remote content.
    pub dir: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Plan {
    pub tx: String,
    pub unit: String,
    pub unit_kind: UnitKind,
    pub ops: Vec<Op>,
    pub conflicts: Vec<ConflictPlan>,
    /// Absolute path of the `index.lock` we hold, if any.
    pub lock: Option<String>,
    /// Actions for reporting.
    pub actions: Vec<PlannedAction>,
}

impl Plan {
    /// Sort operations into the Git-aware apply order:
    /// spelling renames and directories (shallow first) → objects → packed-refs → refs → reflogs →
    /// HEAD/metadata → worktree (hard-link followers after leaders) →
    /// deletions (deep first) → index → object deletions → database-only.
    pub fn sort(&mut self) {
        self.ops.sort_by(|a, b| {
            let ka = (a.phase, sub_key(a), a.path.as_str());
            let kb = (b.phase, sub_key(b), b.path.as_str());
            ka.cmp(&kb)
        });
    }

    pub fn has_disk_ops(&self) -> bool {
        self.ops.iter().any(|o| o.kind != OpKind::DbOnly) || !self.conflicts.is_empty()
    }
}

fn sub_key(o: &Op) -> (i64, u8, u8) {
    let depth = rpath::depth(&o.path) as i64;
    match o.phase {
        Phase::Dirs => (depth, (o.kind != OpKind::Rename) as u8, 0),
        Phase::Writes => (o.rank as i64, o.entry.link.is_some() as u8, 0),
        Phase::Deletes | Phase::ObjectDeletes => (-depth, 0, 0),
        _ => (0, 0, 0),
    }
}

/// Git class of a path in a unit of the given kind (`None` for non-Git units).
pub(crate) fn class_of(kind: UnitKind, path: &str) -> Option<GitClass> {
    (kind == UnitKind::Git).then(|| gitpath::classify(path))
}

pub(crate) fn rank_of(class: Option<GitClass>) -> u8 {
    class.unwrap_or(GitClass::Worktree).apply_rank()
}

#[cfg(test)]
mod tests {
    use super::*;
    use macsync_core::{Kind, VersionVector};

    fn op(path: &str, kind: OpKind, phase: Phase) -> Op {
        let class = class_of(UnitKind::Git, path);
        Op {
            path: path.into(),
            disk: path.into(),
            kind,
            phase,
            rank: rank_of(class),
            entry: Entry::tombstone(path, Kind::File, VersionVector::new()),
            staged: None,
            source: None,
            reason: String::new(),
        }
    }

    #[test]
    fn order_is_git_aware() {
        let mut p = Plan {
            tx: "t".into(),
            unit: "u".into(),
            unit_kind: UnitKind::Git,
            ops: vec![
                op(".git/index", OpKind::Write, Phase::Index),
                op("src/a.rs", OpKind::Write, Phase::Writes),
                op(".git/HEAD", OpKind::Write, Phase::Writes),
                op(".git/refs/heads/main", OpKind::Write, Phase::Writes),
                op(".git/objects/pack/p.idx", OpKind::Write, Phase::Writes),
                op(".git/objects/pack/p.pack", OpKind::Write, Phase::Writes),
                op(".git/objects/ab/cd", OpKind::Write, Phase::Writes),
                op(".git/packed-refs", OpKind::Write, Phase::Writes),
                op("old/deep/x", OpKind::Delete, Phase::Deletes),
                op("old", OpKind::Delete, Phase::Deletes),
                op("src", OpKind::Mkdir, Phase::Dirs),
                op("", OpKind::Mkdir, Phase::Dirs),
                op(".git/objects/ef/01", OpKind::Delete, Phase::ObjectDeletes),
            ],
            conflicts: vec![],
            lock: None,
            actions: vec![],
        };
        p.sort();
        let order: Vec<_> = p.ops.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "",
                "src",
                ".git/objects/ab/cd",
                ".git/objects/pack/p.pack",
                ".git/objects/pack/p.idx",
                ".git/packed-refs",
                ".git/refs/heads/main",
                ".git/HEAD",
                "src/a.rs",
                "old/deep/x",
                "old",
                ".git/index",
                ".git/objects/ef/01",
            ]
        );
    }
}
