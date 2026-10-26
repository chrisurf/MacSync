//! Walking a unit and collecting stat information.
//!
//! The scanner never follows symlinks, never hashes (the engine decides what
//! to hash based on its stat cache) and never descends into nested units.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

use macsync_core::entry::ROOT_UNIT;
use macsync_core::{gitpath, path as rpath, Kind, UnitKind};

use crate::{meta, root_rel, unit_dir, Excludes};

/// One object found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanned {
    /// Canonical (NFC) unit-relative path.
    pub path: String,
    /// Unit-relative path as spelled on disk.
    pub disk: String,
    pub kind: Kind,
    pub mode: u32,
    pub size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    pub ino: u64,
    pub dev: u64,
    pub nlink: u64,
    pub flags: u32,
    pub target: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub items: Vec<Scanned>,
    /// Unit-relative directories that are units of their own.
    pub nested_units: Vec<String>,
    /// Objects that were skipped, with the reason (journaled by the engine).
    pub skipped: Vec<(String, String)>,
}

/// Stat a single path into a [`Scanned`] (no-follow).
pub fn stat(abs: &Path, path: String, disk: String) -> io::Result<Option<Scanned>> {
    let md = fs::symlink_metadata(abs)?;
    Ok(from_metadata(abs, &md, path, disk))
}

fn from_metadata(abs: &Path, md: &fs::Metadata, path: String, disk: String) -> Option<Scanned> {
    let ft = md.file_type();
    let kind = if ft.is_symlink() {
        Kind::Symlink
    } else if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        Kind::File
    } else {
        return None;
    };
    let target = if kind == Kind::Symlink {
        use std::os::unix::ffi::OsStrExt;
        Some(fs::read_link(abs).ok()?.as_os_str().as_bytes().to_vec())
    } else {
        None
    };
    Some(Scanned {
        path,
        disk,
        kind,
        mode: md.mode() & 0o7777,
        size: if kind == Kind::File { md.len() } else { 0 },
        mtime_ns: md.mtime() * 1_000_000_000 + md.mtime_nsec(),
        ctime_ns: md.ctime() * 1_000_000_000 + md.ctime_nsec(),
        ino: md.ino(),
        dev: md.dev(),
        nlink: md.nlink(),
        flags: meta::flags_of(md),
        target,
    })
}

fn special_reason(ft: &fs::FileType) -> &'static str {
    if ft.is_fifo() {
        "fifo"
    } else if ft.is_socket() {
        "socket"
    } else if ft.is_block_device() || ft.is_char_device() {
        "device"
    } else {
        "unsupported file type"
    }
}

/// Scan one unit. A missing unit directory yields an empty result.
pub fn scan_unit(root: &Path, unit: &str, kind: UnitKind, excludes: &Excludes) -> io::Result<ScanResult> {
    let dir = unit_dir(root, unit);
    let mut res = ScanResult::default();
    let md = match fs::symlink_metadata(&dir) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(res),
        Err(e) => return Err(e),
    };
    if !md.is_dir() {
        return Ok(res);
    }
    if unit == ROOT_UNIT {
        walk_dir(unit, kind, excludes, &dir, "", "", false, &mut res)?;
        return Ok(res);
    }
    if let Some(s) = from_metadata(&dir, &md, String::new(), String::new()) {
        res.items.push(s);
    }
    walk_dir(unit, kind, excludes, &dir, "", "", true, &mut res)?;
    Ok(res)
}

#[allow(clippy::too_many_arguments)]
fn walk_dir(
    unit: &str,
    kind: UnitKind,
    excludes: &Excludes,
    abs: &Path,
    canon: &str,
    disk: &str,
    recurse: bool,
    res: &mut ScanResult,
) -> io::Result<()> {
    let rd = match fs::read_dir(abs) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            res.skipped.push((canon.to_string(), format!("unreadable directory: {e}")));
            return Ok(());
        }
    };
    let in_git_dir = canon == ".git" || canon.starts_with(".git/");
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for de in entries {
        let os_name = de.file_name();
        let Some(name) = os_name.to_str() else {
            res.skipped.push((format!("{canon}/{}", os_name.to_string_lossy()), "non UTF-8 file name".into()));
            continue;
        };
        let cname = rpath::nfc(name);
        let cpath = rpath::join(canon, &cname);
        let dpath = rpath::join(disk, name);
        if let Some(prev) = seen.insert(cname.clone(), name.to_string()) {
            res.skipped.push((cpath, format!("normalisation collision with {prev:?}")));
            continue;
        }
        if excludes.is_excluded(&root_rel(unit, &cpath), &cname) {
            continue;
        }
        if kind == UnitKind::Git && gitpath::is_excluded(&cpath) {
            continue;
        }
        let child = de.path();
        let md = match fs::symlink_metadata(&child) {
            Ok(m) => m,
            Err(_) => continue, // vanished during the scan
        };
        let ft = md.file_type();
        if ft.is_dir() && unit == ROOT_UNIT {
            continue; // top-level directories are units of their own
        }
        if ft.is_dir() && !in_git_dir && crate::is_git_dir(&child) {
            res.nested_units.push(cpath);
            continue;
        }
        if meta::raw_flags(&md) & meta::SF_DATALESS != 0 {
            res.skipped.push((cpath, "iCloud placeholder (dataless), not downloaded".into()));
            continue;
        }
        match from_metadata(&child, &md, cpath.clone(), dpath.clone()) {
            Some(s) => {
                let is_dir = s.kind == Kind::Dir;
                res.items.push(s);
                if is_dir && recurse {
                    walk_dir(unit, kind, excludes, &child, &cpath, &dpath, true, res)?;
                }
            }
            None => res.skipped.push((cpath, special_reason(&ft).into())),
        }
    }
    Ok(())
}

/// Discover the top-level units below `root` (non-recursive; nested units are
/// reported by [`scan_unit`]). Always includes the root unit `.`.
pub fn top_level_units(root: &Path, excludes: &Excludes) -> io::Result<Vec<(String, UnitKind)>> {
    let mut out = vec![(ROOT_UNIT.to_string(), UnitKind::Root)];
    let mut names: Vec<_> = fs::read_dir(root)?.filter_map(|e| e.ok()).collect();
    names.sort_by_key(|e| e.file_name());
    for de in names {
        let Some(name) = de.file_name().to_str().map(rpath::nfc) else {
            continue;
        };
        if excludes.is_excluded(&name, &name) {
            continue;
        }
        let p = de.path();
        if fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
            let k = if crate::is_git_dir(&p) { UnitKind::Git } else { UnitKind::Folder };
            out.push((name, k));
        }
    }
    Ok(out)
}

/// Detect the unit kind of an existing unit directory.
pub fn unit_kind_on_disk(root: &Path, unit: &str) -> Option<UnitKind> {
    if unit == ROOT_UNIT {
        return Some(UnitKind::Root);
    }
    let d = unit_dir(root, unit);
    if !fs::symlink_metadata(&d).is_ok_and(|m| m.is_dir()) {
        return None;
    }
    Some(if crate::is_git_dir(&d) { UnitKind::Git } else { UnitKind::Folder })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn setup() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        fs::create_dir_all(r.join("proj/.git/refs/heads")).unwrap();
        fs::write(r.join("proj/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(r.join("proj/.git/index.lock"), "").unwrap();
        fs::write(r.join("proj/a.txt"), "a").unwrap();
        fs::create_dir_all(r.join("proj/empty")).unwrap();
        symlink("a.txt", r.join("proj/link")).unwrap();
        symlink("/does/not/exist", r.join("proj/dead")).unwrap();
        fs::create_dir_all(r.join("proj/vendor/inner/.git")).unwrap();
        fs::write(r.join("proj/.DS_Store"), "x").unwrap();
        fs::create_dir_all(r.join("notes")).unwrap();
        fs::write(r.join("notes/n.md"), "n").unwrap();
        fs::write(r.join("loose.txt"), "l").unwrap();
        fs::create_dir_all(r.join(".macsync/staging")).unwrap();
        d
    }

    #[test]
    fn scans_git_unit() {
        let d = setup();
        let res = scan_unit(d.path(), "proj", UnitKind::Git, &Excludes::default()).unwrap();
        let paths: Vec<_> = res.items.iter().map(|s| s.path.as_str()).collect();
        assert!(paths.contains(&""));
        assert!(paths.contains(&".git/HEAD"));
        assert!(!paths.contains(&".git/index.lock"));
        assert!(paths.contains(&"empty"));
        assert!(!paths.contains(&".DS_Store"));
        let link = res.items.iter().find(|s| s.path == "link").unwrap();
        assert_eq!(link.kind, Kind::Symlink);
        assert_eq!(link.target.as_deref(), Some(&b"a.txt"[..]));
        assert!(paths.contains(&"dead"));
        assert_eq!(res.nested_units, vec!["vendor/inner".to_string()]);
        assert!(!paths.iter().any(|p| p.starts_with("vendor/inner")));
    }

    #[test]
    fn root_unit_is_flat() {
        let d = setup();
        let res = scan_unit(d.path(), ROOT_UNIT, UnitKind::Root, &Excludes::default()).unwrap();
        let paths: Vec<_> = res.items.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, vec!["loose.txt"]);
    }

    #[test]
    fn discovers_top_level_units() {
        let d = setup();
        let units = top_level_units(d.path(), &Excludes::default()).unwrap();
        assert_eq!(
            units,
            vec![(".".into(), UnitKind::Root), ("notes".into(), UnitKind::Folder), ("proj".into(), UnitKind::Git)]
        );
    }

    #[test]
    fn skips_fifo() {
        let d = setup();
        let p = d.path().join("notes/pipe");
        let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
        let res = scan_unit(d.path(), "notes", UnitKind::Folder, &Excludes::default()).unwrap();
        assert!(res.skipped.iter().any(|(p, r)| p == "pipe" && r == "fifo"));
    }

    #[test]
    fn missing_unit_is_empty() {
        let d = setup();
        let res = scan_unit(d.path(), "gone", UnitKind::Folder, &Excludes::default()).unwrap();
        assert!(res.items.is_empty());
    }
}
