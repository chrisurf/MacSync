//! Classification of paths inside a Git unit.
//!
//! The class decides about exclusion (process-local files), the conflict rules
//! and the order in which files are applied on the receiving side.

/// Class of a unit-relative path in a Git unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GitClass {
    /// Loose object `.git/objects/xx/…`.
    LooseObject,
    /// `.git/objects/pack/*.pack`.
    Pack,
    /// `.git/objects/pack/*.idx`.
    PackIndex,
    /// `.git/objects/pack/*.{rev,bitmap,mtimes,promisor,keep}`.
    PackAux,
    /// Everything else below `.git/objects` (e.g. `info/alternates`).
    ObjectOther,
    PackedRefs,
    Ref,
    Reflog,
    Head,
    /// `.git/index`.
    Index,
    /// Files that only reflect the last command and are safe to overwrite with
    /// either side's version (`FETCH_HEAD`, `ORIG_HEAD`, `COMMIT_EDITMSG`, …).
    Ephemeral,
    /// Hooks: synced like metadata but changes are reported.
    Hook,
    /// Other `.git` content: `config`, `info/`, `description`, …
    Meta,
    /// Files that only make sense inside one process: locks, pid files, the
    /// fsmonitor socket, object quarantine directories. Never transferred.
    ProcessLocal,
    /// Not inside `.git`: the working tree.
    Worktree,
}

impl GitClass {
    /// True for paths whose concurrent modification is part of the "Git state"
    /// coherence group (HEAD, refs, index): a conflict in any of them holds back
    /// the whole group.
    pub fn is_git_state(self) -> bool {
        matches!(self, GitClass::PackedRefs | GitClass::Ref | GitClass::Reflog | GitClass::Head | GitClass::Index)
    }

    pub fn is_object(self) -> bool {
        matches!(
            self,
            GitClass::LooseObject | GitClass::Pack | GitClass::PackIndex | GitClass::PackAux | GitClass::ObjectOther
        )
    }

    /// Order in which writes are applied (lower first). The index is always
    /// written last.
    pub fn apply_rank(self) -> u8 {
        match self {
            GitClass::LooseObject => 10,
            GitClass::Pack => 11,
            GitClass::PackIndex => 12,
            GitClass::PackAux => 13,
            GitClass::ObjectOther => 14,
            GitClass::PackedRefs => 20,
            GitClass::Ref => 21,
            GitClass::Reflog => 22,
            GitClass::Head => 30,
            GitClass::Ephemeral | GitClass::Hook | GitClass::Meta => 31,
            GitClass::ProcessLocal => 32,
            GitClass::Worktree => 40,
            GitClass::Index => 90,
        }
    }
}

const EPHEMERAL: &[&str] = &[
    "FETCH_HEAD",
    "ORIG_HEAD",
    "COMMIT_EDITMSG",
    "AUTO_MERGE",
    "MERGE_MSG",
    "MERGE_MODE",
    "SQUASH_MSG",
    "TAG_EDITMSG",
    "description",
    "objects/info/packs",
    "objects/info/commit-graph",
    "logs/refs/stash.lock",
];

/// Classify a unit-relative `path` of a Git unit.
pub fn classify(path: &str) -> GitClass {
    let rest = if path == ".git" {
        return GitClass::Meta;
    } else if let Some(r) = path.strip_prefix(".git/") {
        r
    } else {
        return GitClass::Worktree;
    };

    if is_process_local(rest) {
        return GitClass::ProcessLocal;
    }
    if EPHEMERAL.contains(&rest) || rest.starts_with("objects/info/commit-graphs") {
        return GitClass::Ephemeral;
    }
    if let Some(obj) = rest.strip_prefix("objects/") {
        if let Some(p) = obj.strip_prefix("pack/") {
            if p.ends_with(".pack") {
                return GitClass::Pack;
            }
            if p.ends_with(".idx") {
                return GitClass::PackIndex;
            }
            return GitClass::PackAux;
        }
        let mut it = obj.splitn(2, '/');
        let dir = it.next().unwrap_or("");
        if dir.len() == 2 && dir.bytes().all(|b| b.is_ascii_hexdigit()) && it.next().is_some() {
            return GitClass::LooseObject;
        }
        return GitClass::ObjectOther;
    }
    if rest == "objects" {
        return GitClass::ObjectOther;
    }
    match rest {
        "HEAD" => GitClass::Head,
        "index" => GitClass::Index,
        "packed-refs" => GitClass::PackedRefs,
        _ if rest.starts_with("refs/") || rest == "refs" => GitClass::Ref,
        _ if rest.starts_with("logs/") || rest == "logs" => GitClass::Reflog,
        _ if rest.starts_with("hooks/") => GitClass::Hook,
        _ => GitClass::Meta,
    }
}

fn is_process_local(rest: &str) -> bool {
    let name = crate::path::file_name(rest);
    name.ends_with(".lock")
        || rest == "gc.pid"
        || rest == "gc.log.lock"
        || rest.starts_with("fsmonitor--daemon")
        || rest.starts_with("objects/incoming-")
        || rest.contains("/tmp_obj_")
        || name.starts_with("tmp_obj_")
        || rest.starts_with("objects/pack/tmp_")
        || rest.starts_with("objects/pack/.tmp-")
}

/// Exclusion check for a unit-relative path inside a Git unit, including
/// nested `.git` directories of a worktree file.
pub fn is_excluded(path: &str) -> bool {
    classify(path) == GitClass::ProcessLocal
}

/// Files whose presence means that Git is in the middle of a multi-step
/// operation. While any of them exists the unit is neither captured nor
/// written to.
pub const OPERATION_MARKERS: &[&str] = &[
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];

/// Lock files that signal a running Git command.
pub const LOCK_FILES: &[&str] =
    &["index.lock", "HEAD.lock", "packed-refs.lock", "shallow.lock", "config.lock", "ORIG_HEAD.lock"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert_eq!(classify("src/main.rs"), GitClass::Worktree);
        assert_eq!(classify(".gitignore"), GitClass::Worktree);
        assert_eq!(classify(".git"), GitClass::Meta);
        assert_eq!(classify(".git/HEAD"), GitClass::Head);
        assert_eq!(classify(".git/index"), GitClass::Index);
        assert_eq!(classify(".git/index.lock"), GitClass::ProcessLocal);
        assert_eq!(classify(".git/refs/heads/main"), GitClass::Ref);
        assert_eq!(classify(".git/refs/heads/main.lock"), GitClass::ProcessLocal);
        assert_eq!(classify(".git/packed-refs"), GitClass::PackedRefs);
        assert_eq!(classify(".git/logs/HEAD"), GitClass::Reflog);
        assert_eq!(classify(".git/objects/ab/cdef0123"), GitClass::LooseObject);
        assert_eq!(classify(".git/objects/ab"), GitClass::ObjectOther);
        assert_eq!(classify(".git/objects/pack/pack-1.pack"), GitClass::Pack);
        assert_eq!(classify(".git/objects/pack/pack-1.idx"), GitClass::PackIndex);
        assert_eq!(classify(".git/objects/pack/pack-1.rev"), GitClass::PackAux);
        assert_eq!(classify(".git/objects/pack/tmp_pack_x"), GitClass::ProcessLocal);
        assert_eq!(classify(".git/objects/incoming-abc/x"), GitClass::ProcessLocal);
        assert_eq!(classify(".git/objects/info/packs"), GitClass::Ephemeral);
        assert_eq!(classify(".git/FETCH_HEAD"), GitClass::Ephemeral);
        assert_eq!(classify(".git/hooks/pre-commit"), GitClass::Hook);
        assert_eq!(classify(".git/config"), GitClass::Meta);
        assert_eq!(classify(".git/gc.pid"), GitClass::ProcessLocal);
        assert_eq!(classify(".git/fsmonitor--daemon.ipc"), GitClass::ProcessLocal);
    }

    #[test]
    fn ranks_follow_git_order() {
        let order = [
            GitClass::LooseObject,
            GitClass::Pack,
            GitClass::PackIndex,
            GitClass::PackAux,
            GitClass::PackedRefs,
            GitClass::Ref,
            GitClass::Reflog,
            GitClass::Head,
            GitClass::Worktree,
            GitClass::Index,
        ];
        for w in order.windows(2) {
            assert!(w[0].apply_rank() < w[1].apply_rank(), "{:?} < {:?}", w[0], w[1]);
        }
    }
}
