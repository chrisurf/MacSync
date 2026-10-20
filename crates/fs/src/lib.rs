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

use reposync_core::entry::ROOT_UNIT;

/// Name of RepoSync's private directory inside the root.
pub const PRIVATE_DIR: &str = ".reposync";

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
