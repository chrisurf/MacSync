//! Git awareness: when may a repository be captured or written, how is the
//! index compared, how are reflogs merged, and the few Git commands RepoSync
//! runs itself (none of which ever create history).

pub mod cmd;
pub mod gate;
pub mod index;
pub mod reflog;

pub use gate::{Gate, GateState};

use std::path::{Path, PathBuf};

/// Resolve the Git directory of a working tree: `.git/` or the target of a
/// `.git` file (`gitdir: …`, used by worktrees and submodules).
pub fn resolve_git_dir(work: &Path) -> Option<PathBuf> {
    let dot = work.join(".git");
    let md = std::fs::symlink_metadata(&dot).ok()?;
    if md.is_dir() {
        return Some(dot);
    }
    if md.is_file() {
        let s = std::fs::read_to_string(&dot).ok()?;
        let p = s.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
        let p = Path::new(p);
        let abs = if p.is_absolute() { p.to_path_buf() } else { work.join(p) };
        return abs.is_dir().then_some(abs);
    }
    None
}

/// Length in bytes of object ids in this repository (20 for SHA-1, 32 for
/// SHA-256), read from `extensions.objectformat` in the config.
pub fn object_id_len(git_dir: &Path) -> usize {
    let cfg = std::fs::read_to_string(git_dir.join("config")).unwrap_or_default();
    let sha256 = cfg.lines().any(|l| {
        let l = l.trim().to_ascii_lowercase().replace(' ', "");
        l == "objectformat=sha256"
    });
    if sha256 {
        32
    } else {
        20
    }
}
