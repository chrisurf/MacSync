//! Duplicate finder (read-only): which folders and files exist more than
//! once under the same name, and which copy is the current one.
//!
//! Typical case: a working folder was copied somewhere else at some point
//! (backup, other Mac) and both copies were worked on afterwards. Same-named
//! folders are paired by their relative file paths; every pair gets one of
//! three verdicts:
//!
//! - `identical`: same files, same contents – one copy can go;
//! - `newer`: `a` has everything `b` has, plus new or newer files – `a` is
//!   current, `b` is an old copy;
//! - `diverged`: both have something the other lacks – merge first.
//!
//! Contents are compared by hash, and only where name and size match. "Newer"
//! means a later modification time; equal times with different contents are
//! reported as unclear. Nothing is changed on disk, except by [`apply`]:
//! carrying out a plan the user chose after comparing ALT with NEU.

mod apply;
mod compare;
mod groups;
mod hash;
mod pairs;
mod walk;

use std::io;
use std::path::PathBuf;
use std::time::Instant;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;

pub use apply::{
    apply, apply_with, probe, Action, Done, Event, Expect, ItemKind, Op, Outcome, Plan, Probe, Progress, Size, Start,
    Summary, Trash,
};
pub use compare::{compare, Comparison, Counts, Entry, Folder, FolderStatus, Order, OrderStatus, State};

/// Names never looked at: Mac metadata, Git internals, dependencies, build and
/// cache folders, MacSync's private folder. Folders containing a
/// `CACHEDIR.TAG` (e.g. Rust's `target`) are skipped as well.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".DS_Store",
    ".Spotlight-V100",
    ".Trashes",
    ".fseventsd",
    ".TemporaryItems",
    ".DocumentRevisions-V100",
    "Icon\r",
    ".git",
    "node_modules",
    ".build",
    "DerivedData",
    "__pycache__",
    ".venv",
    ".tox",
    ".next",
    ".gradle",
    ".terraform",
    ".cache",
    ".pytest_cache",
    ".mypy_cache",
    ".macsync",
    ".reposync",
];

/// Modification times closer than this count as equal (FAT/SMB copies round
/// to two seconds).
pub(crate) const SAME_TIME_MS: i64 = 2000;

#[derive(Clone, Debug)]
pub struct Options {
    /// Glob patterns matched against file and folder names.
    pub exclude: Vec<String>,
    /// Skip folders that contain a `CACHEDIR.TAG`.
    pub skip_caches: bool,
    /// Progress lines on stderr.
    pub progress: bool,
    /// [`compare`]: list identical files in `entries` too.
    pub files: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            exclude: DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
            skip_caches: true,
            progress: false,
            files: false,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub roots: Vec<PathBuf>,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    /// Files whose content was read to compare it.
    pub hashed_files: u64,
    pub hashed_bytes: u64,
    /// iCloud placeholders (not downloaded, compared by size and date only).
    pub dataless: u64,
    pub skipped: Vec<Skipped>,
    pub notes: Vec<String>,
    pub elapsed_ms: u64,
    /// Space that can be freed without losing anything (identical or older
    /// folder copies, identical file copies).
    pub reclaimable_bytes: u64,
    pub pairs: Vec<DirPair>,
    pub groups: Vec<FileGroup>,
}

#[derive(Debug, Serialize)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Same files, same contents.
    Identical,
    /// `a` contains everything of `b`, plus new or newer files.
    Newer,
    /// Both contain something the other lacks.
    Diverged,
}

/// Two same-named folders with largely the same relative file paths.
#[derive(Debug, Serialize)]
pub struct DirPair {
    pub verdict: Verdict,
    /// The copy to keep (`newer`), the more recently changed one (`diverged`).
    pub a: DirSide,
    pub b: DirSide,
    pub identical: u64,
    pub identical_bytes: u64,
    /// Same size and date, content not readable (iCloud): assumed identical.
    pub unverified: u64,
    pub only_a: Vec<Item>,
    pub only_b: Vec<Item>,
    pub newer_a: Vec<Changed>,
    pub newer_b: Vec<Changed>,
    /// Different contents with the same modification time.
    pub unclear: Vec<Changed>,
    /// Size of `b` if it can go without losing anything, else 0.
    pub reclaimable_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct DirSide {
    pub path: PathBuf,
    pub display: String,
    pub files: u64,
    pub bytes: u64,
    /// Latest modification time of a file inside.
    pub newest_ms: i64,
    /// Most files were created within a few minutes: the moment the folder
    /// was copied or moved here.
    pub copied: Option<CopyEvent>,
    /// Files modified after the later copy event of the pair.
    pub changed_since_copy: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct CopyEvent {
    pub at_ms: i64,
    pub until_ms: i64,
    pub files: u64,
}

/// A file present on one side only (path relative to the pair's folder).
#[derive(Debug, Serialize)]
pub struct Item {
    pub path: String,
    pub size: u64,
    pub mtime_ms: i64,
}

/// A file present on both sides with different contents.
#[derive(Debug, Serialize)]
pub struct Changed {
    pub path: String,
    pub size_a: u64,
    pub size_b: u64,
    pub mtime_a: i64,
    pub mtime_b: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    /// All copies have the same content.
    Duplicates,
    /// At least two different contents.
    Versions,
}

/// Same-named files outside a reported folder pair.
#[derive(Debug, Serialize)]
pub struct FileGroup {
    pub name: String,
    pub kind: GroupKind,
    /// Newest first.
    pub members: Vec<Member>,
    pub reclaimable_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct Member {
    pub path: PathBuf,
    pub display: String,
    pub size: u64,
    pub mtime_ms: i64,
    pub birth_ms: Option<i64>,
    /// Members with the same number have the same content.
    pub content: usize,
    pub newest: bool,
    /// Also part of a reported folder pair.
    pub paired: bool,
    /// iCloud placeholder (content not compared).
    pub dataless: bool,
}

/// Scan one folder (duplicates inside it) or two folders (compare them, plus
/// duplicates across and inside both).
pub fn scan(roots: &[PathBuf], opts: &Options) -> io::Result<Report> {
    let start = Instant::now();
    if roots.is_empty() || roots.len() > 2 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "pass one or two folders"));
    }
    let abs = roots.iter().map(|r| walk::canonical(r)).collect::<io::Result<Vec<_>>>()?;
    walk::check_roots(&abs)?;
    let tree = walk::walk(&abs, &exclude_set(opts)?, opts.skip_caches, opts.progress)?;
    let todo = hash::candidates(&tree);
    let (hashes, hashed_bytes) = hash::hash_all(&tree, &todo, opts.progress);
    let labels = labels(&abs);
    let ctx = Ctx { t: &tree, h: &hashes, labels: &labels };
    let mut notes = Vec::new();
    let (pairs, paired) = pairs::find(&ctx, &mut notes);
    let groups = groups::find(&ctx, &paired);

    let (mut files, mut dirs, mut bytes, mut dataless) = (0, 0, 0, 0);
    for n in &tree.nodes {
        if n.dir {
            dirs += 1;
        } else {
            files += 1;
            bytes += n.size;
            dataless += u64::from(n.dataless);
        }
    }
    let reclaimable_bytes = pairs.iter().map(|p| p.reclaimable_bytes).sum::<u64>()
        + groups.iter().filter(|g| g.members.iter().all(|m| !m.paired)).map(|g| g.reclaimable_bytes).sum::<u64>();
    Ok(Report {
        roots: abs,
        files,
        dirs: dirs - tree.roots.len() as u64,
        bytes,
        hashed_files: todo.len() as u64,
        hashed_bytes,
        dataless,
        skipped: tree.skipped,
        notes,
        elapsed_ms: start.elapsed().as_millis() as u64,
        reclaimable_bytes,
        pairs,
        groups,
    })
}

pub(crate) fn exclude_set(opts: &Options) -> io::Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in &opts.exclude {
        b.add(Glob::new(p).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?);
    }
    b.build().map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))
}

pub(crate) struct Ctx<'a> {
    pub t: &'a walk::Tree,
    pub h: &'a hash::Hashes,
    labels: &'a [String],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Content {
    Equal,
    Differ,
    /// Same size, but at least one side was not read (iCloud, unreadable).
    Unknown,
}

impl Ctx<'_> {
    pub fn content(&self, a: usize, b: usize) -> Content {
        let (x, y) = (&self.t.nodes[a], &self.t.nodes[b]);
        if x.size != y.size {
            return Content::Differ;
        }
        if x.size == 0 || (x.dev == y.dev && x.ino == y.ino) {
            return Content::Equal;
        }
        match (self.h.get(&a), self.h.get(&b)) {
            (Some(p), Some(q)) if p == q => Content::Equal,
            (Some(_), Some(_)) => Content::Differ,
            _ => Content::Unknown,
        }
    }

    /// Equal, or unknown with the same size and date.
    pub fn same(&self, a: usize, b: usize) -> bool {
        match self.content(a, b) {
            Content::Equal => true,
            Content::Differ => false,
            Content::Unknown => (self.t.nodes[a].mtime_ms - self.t.nodes[b].mtime_ms).abs() <= SAME_TIME_MS,
        }
    }

    /// Path for people: relative to the scanned folder, prefixed with the
    /// folder's name when two folders were scanned.
    pub fn display(&self, i: usize) -> String {
        let n = &self.t.nodes[i];
        match (self.labels.len(), n.rel.is_empty()) {
            (1, true) => ".".into(),
            (1, false) => n.rel.clone(),
            (_, true) => self.labels[n.root].clone(),
            (_, false) => format!("{}/{}", self.labels[n.root], n.rel),
        }
    }
}

/// Folder names of the roots; `[1] name` / `[2] name` if they are equal.
fn labels(roots: &[PathBuf]) -> Vec<String> {
    let names: Vec<String> =
        roots.iter().map(|r| r.file_name().map_or("/".into(), |n| n.to_string_lossy().into_owned())).collect();
    if names.len() == 2 && names[0] == names[1] {
        names.iter().enumerate().map(|(i, n)| format!("[{}] {n}", i + 1)).collect()
    } else {
        names
    }
}
