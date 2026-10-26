//! File system layer: scanning units, hashing, metadata, atomic operations,
//! staging and trash.

pub mod exclude;
pub mod meta;
pub mod ops;
pub mod scan;
pub mod time;

pub use exclude::Excludes;
pub use scan::{scan_unit, ScanResult, Scanned};

use std::path::{Path, PathBuf};

use macsync_core::entry::ROOT_UNIT;

/// Name of MacSync's private directory inside the root.
pub const PRIVATE_DIR: &str = ".macsync";
/// Its name up to 0.2.x (RepoSync). Renamed when the engine opens; always
/// excluded, so a leftover is never synchronised.
pub const LEGACY_PRIVATE_DIR: &str = ".reposync";

/// True for MacSync's private directory (current or legacy name) and
/// everything inside it; `root_rel` is relative to the sync root.
pub fn is_private(root_rel: &str) -> bool {
    [PRIVATE_DIR, LEGACY_PRIVATE_DIR]
        .iter()
        .any(|d| root_rel == *d || root_rel.strip_prefix(d).is_some_and(|r| r.starts_with('/')))
}

/// Absolute directory of a unit.
pub fn unit_dir(root: &Path, unit: &str) -> PathBuf {
    if unit == ROOT_UNIT {
        root.to_path_buf()
    } else {
        root.join(unit)
    }
}

/// Absolute path of a unit-relative path.
pub fn abs_path(root: &Path, unit: &str, rel: &str) -> PathBuf {
    let d = unit_dir(root, unit);
    if rel.is_empty() {
        d
    } else {
        d.join(rel)
    }
}

/// Root-relative path of a unit-relative path (used for exclude matching and
/// display).
pub fn root_rel(unit: &str, rel: &str) -> String {
    match (unit == ROOT_UNIT, rel.is_empty()) {
        (true, _) => rel.to_string(),
        (false, true) => unit.to_string(),
        (false, false) => format!("{unit}/{rel}"),
    }
}

/// True if a directory is a Git working tree (has a `.git` dir or file).
pub fn is_git_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).is_ok_and(|m| m.is_dir() || m.is_file())
}
