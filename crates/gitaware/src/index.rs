//! Semantic hash of `.git/index` (ADR-0008).
//!
//! The index stores machine specific stat data per entry. The semantic hash
//! only covers what Git considers the *content* of the index: for every entry
//! its mode, object id, stage and user-visible flags and its path, plus the
//! `REUC` (resolve undo) extension. Stat data and derived caches (`TREE`,
//! `UNTR`, `FSMN`, `EOIE`, `IEOT`) are ignored.

use macsync_core::Hash;

/// Compute the semantic hash. Returns `None` for formats that are not
/// understood (e.g. split index), in which case the caller falls back to the
/// byte hash.
pub fn semantic_hash(data: &[u8], oid_len: usize) -> Option<Hash> {
    let mut r = Reader { d: data, p: 0 };
    if r.take(4)? != b"DIRC" {
        return None;
    }
    let version = r.u32()?;
    if !(2..=4).contains(&version) {
        return None;
    }
    let count = r.u32()? as usize;
    let mut h = blake3::Hasher::new();
    h.update(b"reposync-git-index-v1\0");
    h.update(&(count as u64).to_le_bytes());
    let mut prev_path: Vec<u8> = Vec::new();
    for _ in 0..count {
        let start = r.p;
        r.take(24)?; // ctime, mtime, dev, ino
        let mode = r.u32()?;
        r.take(12)?; // uid, gid, size
        let oid = r.take(oid_len)?;
        let flags = r.u16()?;
        let ext = if version >= 3 && flags & 0x4000 != 0 { r.u16()? } else { 0 };
        let path: Vec<u8> = if version == 4 {
            let strip = r.varint()? as usize;
            if strip > prev_path.len() {
                return None;
            }
            let suffix = r.cstr()?;
            let mut p = prev_path[..prev_path.len() - strip].to_vec();
            p.extend_from_slice(suffix);
            p
        } else {
            let p = r.cstr()?.to_vec();
            // Entries are padded with 1–8 NUL bytes to a multiple of 8.
            let len = r.p - start;
            let pad = (8 - (len % 8)) % 8;
            r.take(pad)?;
            p
        };
        h.update(&mode.to_le_bytes());
        h.update(oid);
        // assume-valid (0x8000) and stage (0x3000); name length is derived.
        h.update(&(flags & 0xB000).to_le_bytes());
        // skip-worktree (0x4000) and intent-to-add (0x2000).
        h.update(&(ext & 0x6000).to_le_bytes());
        h.update(&(path.len() as u64).to_le_bytes());
        h.update(&path);
        prev_path = path;
    }
    // Extensions, followed by the trailing checksum.
    while r.d.len() - r.p > oid_len {
        let sig = r.take(4)?;
        let size = r.u32()? as usize;
        let body = r.take(size)?;
        match sig {
            b"link" => return None, // split index: content lives elsewhere
            b"REUC" => {
                h.update(b"REUC");
                h.update(&(size as u64).to_le_bytes());
                h.update(body);
            }
            _ => {}
        }
    }
    Some(Hash::from_blake3(h.finalize()))
}

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    fn cstr(&mut self) -> Option<&'a [u8]> {
        let rest = self.d.get(self.p..)?;
        let n = rest.iter().position(|&b| b == 0)?;
        self.p += n + 1;
        Some(&rest[..n])
    }
    /// Git's offset varint (used by index v4 path compression).
    fn varint(&mut self) -> Option<u64> {
        let mut b = *self.take(1)?.first()?;
        let mut v = (b & 0x7f) as u64;
        while b & 0x80 != 0 {
            b = *self.take(1)?.first()?;
            v = v.checked_add(1)?.checked_shl(7)? | (b & 0x7f) as u64;
        }
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::tests::{git, git_available};
    use std::fs;

    fn sem(dir: &std::path::Path) -> Hash {
        semantic_hash(&fs::read(dir.join(".git/index")).unwrap(), 20).expect("parsable index")
    }

    #[test]
    fn stat_only_changes_keep_semantic_hash() {
        if !git_available() {
            return;
        }
        for version in ["2", "3", "4"] {
            let d = tempfile::tempdir().unwrap();
            let w = d.path();
            git(w, &["init", "-q"]);
            git(w, &["config", "index.version", version]);
            fs::create_dir_all(w.join("src/deep")).unwrap();
            fs::write(w.join("src/deep/a.rs"), "a").unwrap();
            fs::write(w.join("src/b.rs"), "b").unwrap();
            fs::write(w.join("README"), "r").unwrap();
            git(w, &["add", "."]);
            if version == "3" {
                git(w, &["update-index", "--skip-worktree", "README"]);
            }
            let before_bytes = fs::read(w.join(".git/index")).unwrap();
            let before = sem(w);
            // Touch every file: stat data changes, content does not.
            std::thread::sleep(std::time::Duration::from_millis(1100));
            for f in ["src/deep/a.rs", "src/b.rs", "README"] {
                fs::write(w.join(f), fs::read(w.join(f)).unwrap()).unwrap();
            }
            git(w, &["update-index", "--refresh", "-q"]);
            git(w, &["status", "--porcelain"]);
            let after_bytes = fs::read(w.join(".git/index")).unwrap();
            assert_ne!(before_bytes, after_bytes, "v{version}: stat data should have changed");
            assert_eq!(before, sem(w), "v{version}");
            // A real change alters the semantic hash.
            fs::write(w.join("src/b.rs"), "changed").unwrap();
            git(w, &["add", "src/b.rs"]);
            assert_ne!(before, sem(w), "v{version}");
        }
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(semantic_hash(b"nope", 20).is_none());
        assert!(semantic_hash(b"DIRC\0\0\0\x02\0\0\0\x05", 20).is_none());
    }
}
