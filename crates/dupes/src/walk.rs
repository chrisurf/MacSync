//! Reading the directory trees into memory (stat only, never content).

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use globset::GlobSet;
use macsync_core::path as rpath;
use macsync_fs::meta;

use crate::Skipped;

pub(crate) struct Node {
    pub abs: PathBuf,
    /// Index into `Tree::roots`.
    pub root: usize,
    /// Path relative to its root (`""` for the root itself).
    pub rel: String,
    /// Name key: NFC, lower case (APFS is case-insensitive).
    pub key: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub depth: usize,
    pub dir: bool,
    pub size: u64,
    pub mtime_ms: i64,
    pub birth_ms: Option<i64>,
    /// iCloud placeholder: content not on disk, never read.
    pub dataless: bool,
    pub dev: u64,
    pub ino: u64,
}

pub(crate) struct Tree {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
    pub skipped: Vec<Skipped>,
}

impl Tree {
    pub fn files(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.nodes.len()).filter(|&i| !self.nodes[i].dir)
    }

    /// True if `a` lies below `b` (or is `b`).
    pub fn within(&self, mut a: usize, b: usize) -> bool {
        loop {
            if a == b {
                return true;
            }
            match self.nodes[a].parent {
                Some(p) => a = p,
                None => return false,
            }
        }
    }

    /// Path of `n` relative to its ancestor `dir`, built from name keys.
    pub fn key_below(&self, dir: usize, n: usize) -> String {
        let mut parts = Vec::new();
        let mut i = n;
        while i != dir {
            parts.push(self.nodes[i].key.as_str());
            i = self.nodes[i].parent.expect("n lies below dir");
        }
        parts.reverse();
        parts.join("/")
    }

    /// Path of `n` relative to its ancestor `dir`, as spelled on disk.
    pub fn rel_below(&self, dir: usize, n: usize) -> String {
        let d = &self.nodes[dir].rel;
        let r = &self.nodes[n].rel;
        if d.is_empty() {
            r.clone()
        } else {
            r[d.len() + 1..].to_string()
        }
    }

    /// All files below `dir` (recursive).
    pub fn files_below(&self, dir: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            for &c in &self.nodes[d].children {
                if self.nodes[c].dir {
                    stack.push(c);
                } else {
                    out.push(c);
                }
            }
        }
        out
    }
}

pub(crate) fn walk(roots: &[PathBuf], exclude: &GlobSet, skip_caches: bool, progress: bool) -> io::Result<Tree> {
    let mut t = Tree { nodes: Vec::new(), roots: Vec::new(), skipped: Vec::new() };
    let mut seen = 0usize;
    for (ri, root) in roots.iter().enumerate() {
        let md = fs::symlink_metadata(root)?;
        if !md.is_dir() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a folder", root.display())));
        }
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let idx = push(&mut t, root.clone(), ri, String::new(), &name, None, &md);
        t.roots.push(idx);
        let mut stack = vec![idx];
        while let Some(d) = stack.pop() {
            let dir = t.nodes[d].abs.clone();
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => {
                    t.skipped.push(Skipped { path: dir, reason: e.to_string() });
                    continue;
                }
            };
            let mut names: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            names.sort_by_key(|e| e.file_name());
            for e in names {
                let name = e.file_name().to_string_lossy().into_owned();
                if exclude.is_match(&name) {
                    continue;
                }
                let abs = e.path();
                // DirEntry::metadata does not follow symlinks.
                let md = match e.metadata() {
                    Ok(m) => m,
                    Err(err) => {
                        t.skipped.push(Skipped { path: abs, reason: err.to_string() });
                        continue;
                    }
                };
                if md.is_dir() && skip_caches && abs.join("CACHEDIR.TAG").is_file() {
                    continue;
                }
                if !(md.is_dir() || md.is_file()) {
                    // Symlinks would create loops and false duplicates.
                    continue;
                }
                let rel = rpath::join(&t.nodes[d].rel, &name);
                let n = push(&mut t, abs, ri, rel, &name, Some(d), &md);
                t.nodes[d].children.push(n);
                if md.is_dir() {
                    stack.push(n);
                }
                seen += 1;
                if progress && seen.is_multiple_of(5000) {
                    eprint!("\rscanning … {seen} entries");
                }
            }
        }
    }
    if progress && seen >= 5000 {
        eprintln!("\rscanning … {seen} entries");
    }
    Ok(t)
}

fn push(
    t: &mut Tree,
    abs: PathBuf,
    root: usize,
    rel: String,
    name: &str,
    parent: Option<usize>,
    md: &fs::Metadata,
) -> usize {
    let depth = parent.map_or(0, |p| t.nodes[p].depth + 1);
    t.nodes.push(Node {
        abs,
        root,
        rel,
        key: rpath::case_key(name),
        parent,
        children: Vec::new(),
        depth,
        dir: md.is_dir(),
        size: if md.is_file() { md.len() } else { 0 },
        mtime_ms: md.mtime() * 1000 + md.mtime_nsec() / 1_000_000,
        birth_ms: md.created().ok().and_then(|c| c.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64),
        dataless: meta::raw_flags(md) & meta::SF_DATALESS != 0,
        dev: md.dev(),
        ino: md.ino(),
    });
    t.nodes.len() - 1
}

/// Reject roots that contain each other: every file would be its own duplicate.
pub(crate) fn check_roots(roots: &[PathBuf]) -> io::Result<()> {
    for (i, a) in roots.iter().enumerate() {
        for b in &roots[i + 1..] {
            if a.starts_with(b) || b.starts_with(a) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} and {} contain each other; pass one of them", a.display(), b.display()),
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn canonical(p: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(p).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", p.display())))
}
