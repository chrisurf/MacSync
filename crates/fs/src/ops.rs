//! Low-level file operations used by the atomic apply: hashing, staging,
//! copy-on-write cloning, trash and the case-sensitivity probe.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use macsync_core::Hash;

/// Stream a file through BLAKE3.
pub fn hash_file(path: &Path) -> io::Result<Hash> {
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(Hash::from_blake3(h.finalize()))
}

/// Read a file completely and return its content if its hash still equals
/// `expected` (optimistic snapshot check, see ADR-0005).
pub fn read_verified(path: &Path, expected: &Hash) -> io::Result<Option<Vec<u8>>> {
    let data = fs::read(path)?;
    Ok((Hash::of(&data) == *expected).then_some(data))
}

/// Read `len` bytes at `offset`.
pub fn read_range(path: &Path, offset: u64, len: u64) -> io::Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut out = Vec::with_capacity(len as usize);
    f.take(len).read_to_end(&mut out)?;
    Ok(out)
}

/// Write `data` to a new file and flush it to stable storage.
pub fn write_new(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(data)?;
    f.sync_data()
}

/// Write `data` to a new file without flushing; call [`sync_paths`] for the
/// whole batch afterwards (bundled durability, one flush per transaction).
pub fn write_unsynced(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(data)
}

/// Make a batch of freshly written files durable. On Linux a single
/// `syncfs` covers the whole volume; elsewhere the files are flushed in
/// parallel.
pub fn sync_paths(paths: &[PathBuf]) -> io::Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let f = File::open(&paths[0])?;
        if unsafe { libc::syncfs(f.as_raw_fd()) } == 0 {
            return Ok(());
        }
    }
    let threads = 8usize.min(paths.len());
    let chunk = paths.len().div_ceil(threads);
    std::thread::scope(|s| {
        let handles: Vec<_> = paths
            .chunks(chunk)
            .map(|c| {
                s.spawn(move || -> io::Result<()> {
                    for p in c {
                        File::open(p)?.sync_data()?;
                    }
                    Ok(())
                })
            })
            .collect();
        for h in handles {
            h.join().map_err(|_| io::Error::other("fsync thread panicked"))??;
        }
        Ok(())
    })
}

/// Copy-on-write clone if the file system supports it (APFS `clonefile`,
/// Btrfs/XFS reflink), otherwise a regular copy.
pub fn clone_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
    reflink_copy::reflink_or_copy(src, dst).map(|_| ())
}

/// Create `dir` and all missing parents.
pub fn ensure_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

/// Flush a directory entry (makes preceding renames durable).
pub fn fsync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Rename `from` to `to`, failing with `AlreadyExists` instead of replacing
/// whatever is at `to` (a plain `rename` silently overwrites files and empty
/// directories).
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let f = CString::new(from.as_os_str().as_bytes())?;
    let t = CString::new(to.as_os_str().as_bytes())?;
    #[cfg(target_os = "macos")]
    let r = unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let r = unsafe { libc::renameat2(libc::AT_FDCWD, f.as_ptr(), libc::AT_FDCWD, t.as_ptr(), libc::RENAME_NOREPLACE) };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let r = {
        if fs::symlink_metadata(to).is_ok() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        unsafe { libc::rename(f.as_ptr(), t.as_ptr()) }
    };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Remove whatever is at `path` (file, symlink or directory tree).
pub fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Move `path` into the trash directory, keeping the relative layout `rel`.
/// Uses `rename` (same volume, O(1)); falls back to copy + remove.
/// Returns the location in the trash, or `None` if nothing existed.
pub fn move_to_trash(path: &Path, trash_dir: &Path, rel: &str) -> io::Result<Option<PathBuf>> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(None);
    }
    crate::meta::unlock_for_write(path);
    let dest = unique(trash_dir.join(if rel.is_empty() { "_unit_root" } else { rel }));
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    match fs::rename(path, &dest) {
        Ok(()) => Ok(Some(dest)),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_tree(path, &dest)?;
            remove_any(path)?;
            Ok(Some(dest))
        }
        Err(e) => Err(e),
    }
}

/// Recursive copy that preserves symlinks (used across volumes only).
pub fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let md = fs::symlink_metadata(src)?;
    if md.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)
    } else if md.is_dir() {
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        clone_or_copy(src, dst)
    }
}

fn unique(p: PathBuf) -> PathBuf {
    if fs::symlink_metadata(&p).is_err() {
        return p;
    }
    for i in 1.. {
        let mut s = p.clone().into_os_string();
        s.push(format!(".{i}"));
        let c = PathBuf::from(s);
        if fs::symlink_metadata(&c).is_err() {
            return c;
        }
    }
    unreachable!()
}

/// True if the volume holding `dir` treats `A` and `a` as the same name.
pub fn is_case_insensitive(dir: &Path) -> io::Result<bool> {
    let probe = dir.join(format!(".macsync-CaseProbe-{}", std::process::id()));
    write_new(&probe, b"")?;
    let lower = dir.join(format!(".macsync-caseprobe-{}", std::process::id()));
    let insensitive = fs::symlink_metadata(&lower).is_ok();
    let _ = fs::remove_file(&probe);
    Ok(insensitive)
}

/// Total size in bytes and available bytes on the volume of `dir`.
pub fn available_space(dir: &Path) -> io::Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(dir.as_os_str().as_bytes())?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st.f_bavail as u64 * st.f_frsize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashing_and_verification() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        write_new(&f, b"hello world").unwrap();
        let h = hash_file(&f).unwrap();
        assert_eq!(h, Hash::of(b"hello world"));
        assert!(read_verified(&f, &h).unwrap().is_some());
        assert!(read_verified(&f, &Hash::of(b"x")).unwrap().is_none());
        assert_eq!(read_range(&f, 6, 5).unwrap(), b"world");
    }

    #[test]
    fn trash_keeps_layout_and_is_unique() {
        let d = tempfile::tempdir().unwrap();
        let trash = d.path().join("trash");
        for i in 0..2 {
            let f = d.path().join("x");
            write_new(&f, format!("{i}").as_bytes()).unwrap();
            let t = move_to_trash(&f, &trash, "a/b/x").unwrap().unwrap();
            assert!(t.starts_with(&trash));
            assert!(!f.exists());
        }
        assert!(trash.join("a/b/x").exists());
        assert!(trash.join("a/b/x.1").exists());
        assert!(move_to_trash(&d.path().join("missing"), &trash, "m").unwrap().is_none());
    }

    #[test]
    fn rename_noreplace_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let (a, b, c) = (d.path().join("a"), d.path().join("b"), d.path().join("c"));
        write_new(&a, b"a").unwrap();
        write_new(&b, b"b").unwrap();
        let e = rename_noreplace(&a, &b).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&b).unwrap(), b"b");
        fs::create_dir(&c).unwrap();
        fs::create_dir(d.path().join("e")).unwrap();
        assert!(rename_noreplace(&c, &d.path().join("e")).is_err());
        rename_noreplace(&a, &d.path().join("x")).unwrap();
        assert!(!a.exists());
    }

    #[test]
    fn clone_copies_content() {
        let d = tempfile::tempdir().unwrap();
        write_new(&d.path().join("a"), b"data").unwrap();
        clone_or_copy(&d.path().join("a"), &d.path().join("b")).unwrap();
        assert_eq!(fs::read(d.path().join("b")).unwrap(), b"data");
        assert!(available_space(d.path()).unwrap() > 0);
        let _ = is_case_insensitive(d.path()).unwrap();
    }
}
