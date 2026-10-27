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

/// Spelling of the root-relative path `rel` as stored on disk (NFC), resolved
/// component by component. On a case-insensitive volume `obsidian` and
/// `Obsidian` both open the same directory; this returns the one name that
/// really exists. `None` if a component does not exist in any spelling.
pub fn spelling_on_disk(root: &Path, rel: &str) -> Option<String> {
    use macsync_core::path as rpath;
    if rel.is_empty() || rel == ROOT_UNIT {
        return Some(rel.to_string());
    }
    let mut dir = root.to_path_buf();
    let mut out = String::new();
    for comp in rel.split('/') {
        let want = rpath::nfc(comp);
        let mut found: Option<(String, std::ffi::OsString)> = None;
        for e in std::fs::read_dir(&dir).ok()?.flatten() {
            let os = e.file_name();
            let Some(name) = os.to_str().map(rpath::nfc) else {
                continue;
            };
            if name == want {
                found = Some((name, os));
                break;
            }
            if found.is_none() && rpath::case_key(&name) == rpath::case_key(&want) {
                found = Some((name, os));
            }
        }
        let (name, os) = found?;
        dir.push(os);
        out = rpath::join(&out, &name);
    }
    Some(out)
}

/// True if a directory is a Git working tree (has a `.git` dir or file).
pub fn is_git_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).is_ok_and(|m| m.is_dir() || m.is_file())
}
