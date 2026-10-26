//! Metadata: permission bits, modification time, extended attributes and BSD
//! flags. All functions operate on the link itself and never follow symlinks.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use macsync_core::Kind;

/// macOS quarantine attribute (configurable stripping).
pub const QUARANTINE: &str = "com.apple.quarantine";
/// macOS provenance attribute (host-local, never synchronised).
pub const PROVENANCE: &str = "com.apple.provenance";

/// Read all extended attributes of `path` (no-follow), sorted by name.
/// Attributes that cannot be read are skipped.
pub fn read_xattrs(path: &Path) -> Vec<(String, Vec<u8>)> {
    let Ok(names) = xattr::list(path) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<u8>)> = names
        .filter_map(|n| {
            let name = n.to_str()?.to_string();
            // Host-local: Linux security/system namespaces, and macOS
            // provenance tracking (set by the kernel per writing app; it can
            // be neither copied meaningfully nor removed).
            if name.starts_with("security.")
                || name.starts_with("system.")
                || name.starts_with("trusted.")
                || name == PROVENANCE
            {
                return None;
            }
            let v = xattr::get(path, &n).ok()??;
            Some((name, v))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Make the extended attributes of `path` equal to `want`.
pub fn write_xattrs(path: &Path, want: &[(String, Vec<u8>)], strip_quarantine: bool) -> io::Result<()> {
    let have = read_xattrs(path);
    for (k, _) in &have {
        if !want.iter().any(|(w, _)| w == k) {
            let _ = xattr::remove(path, k);
        }
    }
    for (k, v) in want {
        if strip_quarantine && k == QUARANTINE {
            let _ = xattr::remove(path, k);
            continue;
        }
        if have.iter().any(|(h, hv)| h == k && hv == v) {
            continue;
        }
        if let Err(e) = xattr::set(path, k, v) {
            // Unsupported on this file system or for this file type: not fatal.
            tracing::debug!(path = %path.display(), attr = %k, error = %e, "cannot set xattr");
        }
    }
    Ok(())
}

pub fn set_mode(path: &Path, kind: Kind, mode: u32) -> io::Result<()> {
    if kind == Kind::Symlink {
        return Ok(());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
}

pub fn set_mtime(path: &Path, kind: Kind, mtime_ns: i64) -> io::Result<()> {
    let t = filetime::FileTime::from_unix_time(
        mtime_ns.div_euclid(1_000_000_000),
        mtime_ns.rem_euclid(1_000_000_000) as u32,
    );
    if kind == Kind::Symlink {
        filetime::set_symlink_file_times(path, t, t)
    } else {
        filetime::set_file_mtime(path, t)
    }
}

/// BSD flags of a file (macOS); always 0 elsewhere.
#[cfg(target_os = "macos")]
pub fn flags_of(md: &fs::Metadata) -> u32 {
    use std::os::macos::fs::MetadataExt;
    // Only user-settable flags; the kernel-managed ones (e.g. compression) are
    // reproduced by the file system itself.
    md.st_flags() & USER_FLAGS
}

#[cfg(not(target_os = "macos"))]
pub fn flags_of(_md: &fs::Metadata) -> u32 {
    0
}

/// `UF_NODUMP | UF_IMMUTABLE | UF_APPEND | UF_OPAQUE | UF_HIDDEN`.
pub const USER_FLAGS: u32 = 0x0000_0001 | 0x0000_0002 | 0x0000_0004 | 0x0000_0008 | 0x0000_8000;
/// `UF_IMMUTABLE` ("uchg"): must be cleared before writing and restored after.
pub const UF_IMMUTABLE: u32 = 0x0000_0002;
/// `SF_DATALESS`: iCloud placeholder whose content is not on disk.
pub const SF_DATALESS: u32 = 0x4000_0000;

#[cfg(target_os = "macos")]
pub fn raw_flags(md: &fs::Metadata) -> u32 {
    use std::os::macos::fs::MetadataExt;
    md.st_flags()
}

#[cfg(not(target_os = "macos"))]
pub fn raw_flags(_md: &fs::Metadata) -> u32 {
    0
}

/// Set BSD flags (macOS, no-follow). No-op elsewhere.
#[cfg(target_os = "macos")]
pub fn set_flags(path: &Path, flags: u32) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes())?;
    // lchflags does not follow symlinks.
    let r = unsafe { libc::lchflags(c.as_ptr(), flags as _) };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_flags(_path: &Path, _flags: u32) -> io::Result<()> {
    Ok(())
}

/// Clear the immutable flag of an existing path so that it can be replaced.
pub fn unlock_for_write(path: &Path) {
    if let Ok(md) = fs::symlink_metadata(path) {
        let f = raw_flags(&md);
        if f & UF_IMMUTABLE != 0 {
            let _ = set_flags(path, f & !UF_IMMUTABLE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xattr_roundtrip_if_supported() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        fs::write(&f, b"x").unwrap();
        if xattr::set(&f, "user.probe", b"1").is_err() {
            eprintln!("xattrs unsupported here; skipping");
            return;
        }
        let want = vec![("user.color".to_string(), b"red".to_vec())];
        write_xattrs(&f, &want, false).unwrap();
        assert_eq!(read_xattrs(&f), want);
    }

    #[test]
    fn mode_and_mtime() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        fs::write(&f, b"x").unwrap();
        set_mode(&f, Kind::File, 0o751).unwrap();
        set_mtime(&f, Kind::File, 1_000_000_000_123).unwrap();
        let md = fs::metadata(&f).unwrap();
        assert_eq!(md.permissions().mode() & 0o7777, 0o751);
        use std::os::unix::fs::MetadataExt;
        assert_eq!(md.mtime(), 1000);
    }
}
