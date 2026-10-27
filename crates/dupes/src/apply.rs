//! Carrying out a reconcile plan (see `docs/reconcile.md`): files and
//! folders of ALT copied or moved into NEU, NEU files replaced by their ALT
//! version, or both versions kept side by side.
//!
//! Nothing is ever overwritten or deleted for good: copies are written to a
//! temporary file next to the target, confirmed by BLAKE3 and renamed without
//! replacing; replaced NEU files and moved ALT sources go to the macOS Trash.
//! Every action first checks that its files are still as at the comparison.
//! Progress is reported while copying; a cancel stops within 1 MB and
//! removes the unfinished copy.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use macsync_core::{path as rpath, Hash, Kind};
use macsync_fs::{meta, ops, time};
use serde::{Deserialize, Serialize};

use crate::{walk, DEFAULT_EXCLUDES};

#[derive(Debug, Default, Deserialize)]
pub struct Plan {
    pub actions: Vec<Action>,
}

#[derive(Debug, Deserialize)]
pub struct Action {
    pub op: Op,
    #[serde(default)]
    pub kind: ItemKind,
    /// Relative to ALT.
    pub path: String,
    /// Relative to NEU; absent: `path`.
    #[serde(default)]
    pub to: Option<String>,
    /// The ALT file as seen by the comparison.
    #[serde(default)]
    pub expect_old: Option<Expect>,
    /// The NEU file as seen by the comparison.
    #[serde(default)]
    pub expect_new: Option<Expect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Copy to a target that does not exist.
    Copy,
    /// Copy, then put the ALT source into the Trash.
    Move,
    /// Put the NEU file into the Trash, then copy the ALT file to its place.
    Replace,
    /// Copy next to the NEU file as `name (ALT YYYY-MM-DD).ext`.
    KeepBoth,
    /// Put the ALT file or folder into the Trash, only if NEU holds an
    /// identical copy of every file in it (BLAKE3).
    TrashOld,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    #[default]
    File,
    /// A whole folder (`copy`, `move` and `trash_old` only).
    Dir,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct Expect {
    pub size: u64,
    pub mtime_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Done {
    Done,
    /// Not carried out because something changed since the comparison.
    Skipped,
    Failed,
    /// Stopped by a cancel before it was finished (or started).
    Cancelled,
}

/// Result of one action, reported as soon as it is finished.
#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub index: usize,
    pub path: String,
    /// Where it went in NEU (for `keep_both` the name actually chosen).
    pub to: String,
    pub result: Done,
    pub error: Option<String>,
    /// Location in the Trash of the moved ALT source or replaced NEU file.
    pub trashed: Option<String>,
    /// `cloud_unavailable`: Google Drive/iCloud did not deliver a placeholder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    /// Bytes copied.
    #[serde(skip)]
    pub bytes: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub summary: bool,
    pub done: u64,
    pub skipped: u64,
    pub failed: u64,
    pub bytes: u64,
    pub cancelled: bool,
}

/// Sizes of all actions, reported before the first one starts.
#[derive(Debug, Serialize)]
pub struct Start {
    pub start: bool,
    pub bytes_total: u64,
    pub cloud_files: u64,
    pub cloud_bytes: u64,
    pub actions: Vec<Size>,
}

#[derive(Debug, Serialize)]
pub struct Size {
    pub index: usize,
    /// Bytes to copy (a folder: all files in it).
    pub bytes: u64,
    pub files: u64,
    /// Placeholders whose content is still in the cloud.
    pub cloud_files: u64,
    pub cloud_bytes: u64,
}

/// Result of `probe`: sizes of a plan and whether the cloud delivers.
#[derive(Debug, Serialize)]
pub struct Probe {
    pub probe: bool,
    pub bytes_total: u64,
    pub files: u64,
    pub cloud_files: u64,
    pub cloud_bytes: u64,
    /// `None`: no placeholders in the plan.
    pub cloud_ok: Option<bool>,
    pub error: Option<String>,
    pub actions: Vec<Size>,
}

/// Bytes copied so far within action `index`.
#[derive(Debug, Serialize)]
pub struct Progress {
    pub progress: bool,
    pub index: usize,
    pub bytes_done: u64,
    pub bytes: u64,
    /// The file being copied, relative to ALT.
    pub file: String,
}

/// What `apply_with` reports, in this order: `Start`, then per action any
/// number of `Progress` and one `Outcome`.
#[derive(Debug)]
pub enum Event<'a> {
    Start(&'a Start),
    Progress(&'a Progress),
    Outcome(&'a Outcome),
}

/// Where moved and replaced files go.
#[derive(Clone, Debug)]
pub enum Trash {
    /// The macOS Trash: `/usr/bin/trash` (macOS 15+, "Put Back" works),
    /// else a rename into `~/.Trash`.
    System,
    /// A plain folder (tests).
    Dir(PathBuf),
}

impl Trash {
    /// `MACSYNC_TRASH_DIR` if set, else the macOS Trash.
    pub fn from_env() -> Trash {
        match std::env::var_os("MACSYNC_TRASH_DIR") {
            Some(d) if !d.is_empty() => Trash::Dir(d.into()),
            _ => Trash::System,
        }
    }

    /// Move `path` (file or folder) into the trash; returns where it is now.
    pub fn put(&self, path: &Path) -> io::Result<PathBuf> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let into =
            |dir: &Path| ops::move_to_trash(path, dir, &name)?.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound));
        match self {
            Trash::Dir(d) => into(d),
            Trash::System if Path::new(SYSTEM_TRASH).is_file() => {
                let out = Command::new(SYSTEM_TRASH).arg("-v").arg(path).output()?;
                let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
                if !out.status.success() || fs::symlink_metadata(path).is_ok() {
                    return Err(io::Error::other(format!("trash: {}", text.trim())));
                }
                // `# Moved "/a/b" to "/Users/x/.Trash/b"`
                let at = text.lines().find_map(|l| l.rsplit_once("\" to \"")).map(|(_, t)| t.trim_end_matches('"'));
                Ok(at.map_or_else(|| home_trash().join(&name), PathBuf::from))
            }
            Trash::System => into(&home_trash()),
        }
    }
}

const SYSTEM_TRASH: &str = "/usr/bin/trash";

fn home_trash() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".Trash")
}

/// Why an action was not carried out.
enum Stop {
    Skip(String),
    Fail(String),
    /// A placeholder could not be read: the cloud app does not deliver.
    Cloud(String),
    Cancel,
}

impl From<io::Error> for Stop {
    fn from(e: io::Error) -> Stop {
        if e.get_ref().is_some_and(|r| r.is::<Cancelled>()) {
            Stop::Cancel
        } else if e.get_ref().is_some_and(|r| r.is::<CloudUnavailable>()) {
            Stop::Cloud(e.to_string())
        } else {
            Stop::Fail(e.to_string())
        }
    }
}

/// Error payload: reading a cloud placeholder failed.
#[derive(Debug)]
struct CloudUnavailable(String);

impl std::fmt::Display for CloudUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CloudUnavailable {}

fn is_cloud(md: &fs::Metadata) -> bool {
    meta::raw_flags(md) & meta::SF_DATALESS != 0
}

/// A read error of the placeholder `src` (`rel` in ALT) as `CloudUnavailable`.
fn cloud_error(rel: &Path, e: io::Error) -> io::Error {
    if e.get_ref().is_some_and(|r| r.is::<Cancelled>()) {
        return e;
    }
    io::Error::other(CloudUnavailable(format!("Google Drive/iCloud did not deliver {}: {e}", rel.display())))
}

/// Error payload of a copy stopped by a cancel.
#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Cancel flag and progress reporting while one action copies.
struct Ctx<'a> {
    cancel: &'a AtomicBool,
    old_root: &'a Path,
    index: usize,
    bytes: u64,
    done: u64,
    last: Option<Instant>,
    on: &'a mut dyn FnMut(Event),
}

impl Ctx<'_> {
    fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(io::Error::other(Cancelled))
        } else {
            Ok(())
        }
    }

    /// `n` more bytes of `file` copied; reports at most five times a second.
    fn advance(&mut self, file: &Path, n: u64) {
        self.done += n;
        if self.last.is_some_and(|t| t.elapsed() < Duration::from_millis(200)) {
            return;
        }
        self.last = Some(Instant::now());
        let rel = file.strip_prefix(self.old_root).unwrap_or(file);
        let p = Progress {
            progress: true,
            index: self.index,
            bytes_done: self.done,
            bytes: self.bytes,
            file: rel.to_string_lossy().into_owned(),
        };
        (self.on)(Event::Progress(&p));
    }
}

const CHANGED: &str = "changed since the comparison";

struct Ran {
    to: String,
    bytes: u64,
    trashed: Option<PathBuf>,
}

/// Carry out `plan`, action by action; `on` gets each result as soon as it is
/// finished. Fails only if the folders themselves are unusable.
pub fn apply(old: &Path, new: &Path, plan: &Plan, trash: &Trash, mut on: impl FnMut(&Outcome)) -> io::Result<Summary> {
    apply_with(old, new, plan, trash, &AtomicBool::new(false), |e| {
        if let Event::Outcome(o) = e {
            on(o)
        }
    })
}

/// Like `apply`, with sizes and progress reported and stopped as soon as
/// `cancel` is set: the running action and all after it end as `Cancelled`.
pub fn apply_with(
    old: &Path,
    new: &Path,
    plan: &Plan,
    trash: &Trash,
    cancel: &AtomicBool,
    mut on: impl FnMut(Event),
) -> io::Result<Summary> {
    let roots = [walk::canonical(old)?, walk::canonical(new)?];
    walk::check_roots(&roots)?;
    let sizes: Vec<Size> = plan.actions.iter().enumerate().map(|(index, a)| scan(&roots[0], index, a).0).collect();
    let start = Start {
        start: true,
        bytes_total: sizes.iter().map(|s| s.bytes).sum(),
        cloud_files: sizes.iter().map(|s| s.cloud_files).sum(),
        cloud_bytes: sizes.iter().map(|s| s.cloud_bytes).sum(),
        actions: sizes,
    };
    on(Event::Start(&start));
    let mut sum = Summary { summary: true, ..Default::default() };
    // Once the cloud failed to deliver, later placeholders are not tried.
    let mut cloud_down: Option<String> = None;
    for (index, a) in plan.actions.iter().enumerate() {
        let to = a.to.clone().unwrap_or_else(|| a.path.clone());
        let mut o = Outcome {
            index,
            path: a.path.clone(),
            to: to.clone(),
            result: Done::Done,
            error: None,
            trashed: None,
            code: None,
            bytes: 0,
        };
        let res = if cancel.load(Ordering::Relaxed) {
            Err(Stop::Cancel)
        } else if let Some(m) = cloud_down.as_ref().filter(|_| start.actions[index].cloud_files > 0) {
            Err(Stop::Cloud(format!("not tried, the cloud did not deliver before: {m}")))
        } else {
            let mut ctx = Ctx {
                cancel,
                old_root: &roots[0],
                index,
                bytes: start.actions[index].bytes,
                done: 0,
                last: None,
                on: &mut on,
            };
            run(&roots, a, &to, trash, &mut ctx)
        };
        match res {
            Ok(r) => {
                o.to = r.to;
                o.bytes = r.bytes;
                o.trashed = r.trashed.map(|p| p.to_string_lossy().into_owned());
                sum.done += 1;
                sum.bytes += r.bytes;
            }
            Err(Stop::Skip(m)) => {
                o.result = Done::Skipped;
                o.error = Some(m);
                sum.skipped += 1;
            }
            Err(Stop::Fail(m)) => {
                o.result = Done::Failed;
                o.error = Some(m);
                sum.failed += 1;
            }
            Err(Stop::Cloud(m)) => {
                o.result = Done::Failed;
                o.code = Some("cloud_unavailable");
                cloud_down.get_or_insert_with(|| m.clone());
                o.error = Some(m);
                sum.failed += 1;
            }
            Err(Stop::Cancel) => {
                o.result = Done::Cancelled;
                sum.cancelled = true;
            }
        }
        on(Event::Outcome(&o));
    }
    Ok(sum)
}

/// Bytes and files an action will copy, by `stat` only (0 if unreadable),
/// and its smallest cloud placeholder.
fn scan(old_root: &Path, index: usize, a: &Action) -> (Size, Option<(u64, PathBuf)>) {
    let mut s = Size { index, bytes: 0, files: 0, cloud_files: 0, cloud_bytes: 0 };
    let mut smallest: Option<(u64, PathBuf)> = None;
    if rpath::validate(&a.path).is_err() {
        return (s, smallest);
    }
    let mut stack = vec![old_root.join(&a.path)];
    while let Some(p) = stack.pop() {
        let Ok(md) = fs::symlink_metadata(&p) else { continue };
        if md.is_file() {
            s.bytes += md.len();
            s.files += 1;
            if is_cloud(&md) {
                s.cloud_files += 1;
                s.cloud_bytes += md.len();
                // An empty placeholder reads without the cloud: no test.
                if md.len() > 0 && smallest.as_ref().is_none_or(|(n, _)| md.len() < *n) {
                    smallest = Some((md.len(), p));
                }
            }
        } else if md.is_dir() {
            stack
                .extend(fs::read_dir(&p).into_iter().flatten().flatten().map(|e| e.path()).filter(|c| !rebuildable(c)));
        }
    }
    (s, smallest)
}

/// Sizes of `plan` and whether the cloud delivers its placeholders: reads the
/// first byte of the smallest non-empty one (which downloads it). Changes
/// nothing else.
pub fn probe(old: &Path, plan: &Plan) -> io::Result<Probe> {
    let root = walk::canonical(old)?;
    let mut p = Probe {
        probe: true,
        bytes_total: 0,
        files: 0,
        cloud_files: 0,
        cloud_bytes: 0,
        cloud_ok: None,
        error: None,
        actions: Vec::new(),
    };
    let mut smallest: Option<(u64, PathBuf)> = None;
    for (index, a) in plan.actions.iter().enumerate() {
        let (s, small) = scan(&root, index, a);
        p.bytes_total += s.bytes;
        p.files += s.files;
        p.cloud_files += s.cloud_files;
        p.cloud_bytes += s.cloud_bytes;
        if let Some(c) = small.filter(|c| smallest.as_ref().is_none_or(|m| c.0 < m.0)) {
            smallest = Some(c);
        }
        p.actions.push(s);
    }
    if let Some((_, file)) = smallest {
        let read = File::open(&file).and_then(|mut f| f.read(&mut [0u8; 1]));
        p.cloud_ok = Some(read.is_ok());
        if let Err(e) = read {
            p.error = Some(cloud_error(file.strip_prefix(&root).unwrap_or(&file), e).to_string());
        }
    }
    Ok(p)
}

fn run(roots: &[PathBuf; 2], a: &Action, to: &str, trash: &Trash, ctx: &mut Ctx) -> Result<Ran, Stop> {
    for p in [a.path.as_str(), to] {
        rpath::validate(p).map_err(Stop::Fail)?;
        if p.is_empty() {
            return Err(Stop::Fail("empty path".into()));
        }
    }
    if a.kind == ItemKind::Dir && !matches!(a.op, Op::Copy | Op::Move | Op::TrashOld) {
        return Err(Stop::Fail("folders can only be copied, moved or put into the Trash".into()));
    }
    let src = inside(&roots[0], &a.path)?;
    let dst = inside(&roots[1], to)?;
    let md = match fs::symlink_metadata(&src) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Stop::Skip("no longer in ALT".into())),
        Err(e) => return Err(e.into()),
    };
    if md.file_type().is_symlink() {
        return Err(Stop::Fail("is a symbolic link".into()));
    }
    if md.is_dir() != (a.kind == ItemKind::Dir) {
        return Err(Stop::Skip(CHANGED.into()));
    }
    let new_md = fs::symlink_metadata(&dst).ok();
    if a.kind == ItemKind::File {
        if a.expect_old.is_some_and(|e| !matches(&md, e)) {
            return Err(Stop::Skip(CHANGED.into()));
        }
        if let Some(e) = a.expect_new {
            if !new_md.as_ref().is_some_and(|m| m.is_file() && matches(m, e)) {
                return Err(Stop::Skip(CHANGED.into()));
            }
        }
    }
    let parent = dst.parent().expect("below NEU").to_path_buf();
    let mut ran = Ran { to: to.to_string(), bytes: 0, trashed: None };
    match a.op {
        Op::Copy | Op::Move => {
            if new_md.is_some() {
                return Err(Stop::Skip("already exists in NEU".into()));
            }
            fs::create_dir_all(&parent)?;
            let tmp = temp_path(&parent, &dst);
            let staged = if md.is_dir() { copy_dir(&src, &tmp, ctx) } else { copy_file(&src, &tmp, ctx) };
            ran.bytes = settle(staged, &tmp, &dst)?;
            if a.op == Op::Move {
                let t =
                    trash.put(&src).map_err(|e| Stop::Fail(format!("copied, but ALT not moved to the Trash: {e}")))?;
                ran.trashed = Some(t);
            }
        }
        Op::Replace => {
            if !new_md.is_some_and(|m| m.is_file()) {
                return Err(Stop::Skip("not a file in NEU any more".into()));
            }
            let tmp = temp_path(&parent, &dst);
            ran.bytes = copy_file(&src, &tmp, ctx).inspect_err(|_| drop(fs::remove_file(&tmp)))?;
            let t = trash.put(&dst).inspect_err(|_| drop(fs::remove_file(&tmp)))?;
            if let Err(e) = ops::rename_noreplace(&tmp, &dst) {
                let _ = fs::remove_file(&tmp);
                return Err(Stop::Fail(format!("{e}; the NEU file is in the Trash: {}", t.display())));
            }
            let _ = ops::fsync_dir(&parent);
            ran.trashed = Some(t);
        }
        Op::KeepBoth => {
            fs::create_dir_all(&parent)?;
            let tmp = temp_path(&parent, &dst);
            ran.bytes = copy_file(&src, &tmp, ctx).inspect_err(|_| drop(fs::remove_file(&tmp)))?;
            let date = &time::local(mtime_ms(&md))[..10];
            let name = dst.file_name().expect("validated").to_string_lossy().into_owned();
            let (stem, ext) = match name.rsplit_once('.') {
                Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
                _ => (name.clone(), String::new()),
            };
            for n in 1.. {
                let tag = if n == 1 { format!("ALT {date}") } else { format!("ALT {date} {n}") };
                let cand = format!("{stem} ({tag}){ext}");
                match ops::rename_noreplace(&tmp, &parent.join(&cand)) {
                    Ok(()) => {
                        ran.to = rpath::join(rpath::parent(to).unwrap_or(""), &cand);
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => {
                        let _ = fs::remove_file(&tmp);
                        return Err(e.into());
                    }
                }
            }
            let _ = ops::fsync_dir(&parent);
        }
        Op::TrashOld => {
            if new_md.is_none() {
                return Err(Stop::Skip("not in NEU".into()));
            }
            same_in_new(&src, &dst, ctx)?;
            ran.trashed = Some(trash.put(&src).map_err(|e| Stop::Fail(format!("not moved to the Trash: {e}")))?);
        }
    }
    Ok(ran)
}

/// Every file below `src` (or `src` itself) has an identical copy at the
/// same place below `dst`; extra files in NEU do not matter.
fn same_in_new(src: &Path, dst: &Path, ctx: &mut Ctx) -> Result<(), Stop> {
    let md = fs::symlink_metadata(src)?;
    let root = ctx.old_root;
    let rel = || src.strip_prefix(root).unwrap_or(src).display().to_string();
    let Ok(new_md) = fs::symlink_metadata(dst) else {
        return Err(not_same(rel(), "is missing in NEU"));
    };
    if md.file_type().is_symlink() {
        if !new_md.file_type().is_symlink() || fs::read_link(src)? != fs::read_link(dst)? {
            return Err(not_same(rel(), "differs in NEU"));
        }
    } else if md.is_dir() {
        if !new_md.is_dir() {
            return Err(not_same(rel(), "is not a folder in NEU"));
        }
        for e in fs::read_dir(src)? {
            let e = e?;
            if !rebuildable(&e.path()) {
                same_in_new(&e.path(), &dst.join(e.file_name()), ctx)?;
            }
        }
    } else if md.is_file()
        && (!new_md.is_file()
            || new_md.len() != md.len()
            || hash_with(src, ctx).map_err(|e| if is_cloud(&md) { cloud_error(Path::new(&rel()), e) } else { e })?
                != ops::hash_file(dst)?)
    {
        return Err(not_same(rel(), "differs in NEU"));
    }
    Ok(())
}

/// Why `trash_old` keeps `rel`; inside `.git` the history itself differs.
fn not_same(rel: String, what: &str) -> Stop {
    match rel.split_once(".git/").or_else(|| rel.strip_suffix(".git").map(|r| (r, ""))) {
        Some((repo, _)) if repo.is_empty() || repo.ends_with('/') => Stop::Skip(format!(
            "{repo}.git differs in NEU: ALT may have commits, branches or stashes that NEU lacks – compare with git"
        )),
        _ => Stop::Skip(format!("{rel} {what}")),
    }
}

/// Left out of folder copies and checks: what the comparison ignores and
/// tools rebuild (`node_modules`, `.venv`, caches …). `.git` is kept: its
/// history cannot be rebuilt.
fn rebuildable(p: &Path) -> bool {
    let name = p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    (name != ".git" && DEFAULT_EXCLUDES.contains(&name.as_ref()))
        || (fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) && p.join("CACHEDIR.TAG").is_file())
}

/// BLAKE3 of a file in 1 MB chunks, with progress and cancel in between.
fn hash_with(path: &Path, ctx: &mut Ctx) -> io::Result<Hash> {
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    ctx.advance(path, 0);
    loop {
        ctx.check()?;
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        h.update(&buf[..n]);
        ctx.advance(path, n as u64);
    }
    Ok(Hash::from_blake3(h.finalize()))
}

/// `rel` below `root`, refusing anything that leads outside it (symlinked
/// folders on the way).
fn inside(root: &Path, rel: &str) -> Result<PathBuf, Stop> {
    let p = root.join(rel);
    let mut dir = p.parent();
    while let Some(d) = dir {
        match fs::canonicalize(d) {
            Ok(c) if c.starts_with(root) => return Ok(p),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::NotFound => dir = d.parent(),
            Err(e) => return Err(e.into()),
        }
    }
    Err(Stop::Fail(format!("{rel} leads outside {}", root.display())))
}

/// Rename the staged copy `tmp` to `dst` without replacing anything.
fn settle(staged: io::Result<u64>, tmp: &Path, dst: &Path) -> Result<u64, Stop> {
    let bytes = staged.inspect_err(|_| drop(ops::remove_any(tmp)))?;
    match ops::rename_noreplace(tmp, dst) {
        Ok(()) => {
            let _ = ops::fsync_dir(dst.parent().expect("below NEU"));
            Ok(bytes)
        }
        Err(e) => {
            let _ = ops::remove_any(tmp);
            if e.kind() == io::ErrorKind::AlreadyExists {
                Err(Stop::Skip("already exists in NEU".into()))
            } else {
                Err(e.into())
            }
        }
    }
}

fn matches(md: &fs::Metadata, e: Expect) -> bool {
    md.len() == e.size && mtime_ms(md) == e.mtime_ms
}

/// Same rounding as the comparison.
fn mtime_ms(md: &fs::Metadata) -> i64 {
    md.mtime() * 1000 + md.mtime_nsec() / 1_000_000
}

/// Hidden, unique name in `dir` for staging the copy of `dst` (its name cut
/// short, so that long names still fit the 255 bytes of a file name).
fn temp_path(dir: &Path, dst: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let mut name = dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut cut = name.len().min(120);
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    name.truncate(cut);
    let n = N.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{name}.macsync-{}-{n}.tmp", std::process::id()))
}

/// Copy one file to the new path `dst` with date, permissions and extended
/// attributes, confirmed by BLAKE3 and flushed. Returns its size.
fn copy_file(src: &Path, dst: &Path, ctx: &mut Ctx) -> io::Result<u64> {
    ctx.check()?;
    let md = fs::symlink_metadata(src)?;
    let rel = src.strip_prefix(ctx.old_root).unwrap_or(src).to_path_buf();
    let want = stream(src, dst, ctx).map_err(|e| if is_cloud(&md) { cloud_error(&rel, e) } else { e })?;
    meta::unlock_for_write(dst);
    copy_meta(src, dst, &md, Kind::File)?;
    if ops::hash_file(dst)? != want {
        return Err(io::Error::other(format!("the copy of {} does not match the original", src.display())));
    }
    File::open(dst)?.sync_all()?;
    Ok(md.len())
}

/// Copy the content in 1 MB chunks (progress and cancel in between, also
/// within large files) and return the BLAKE3 hash of what was read.
fn stream(src: &Path, dst: &Path, ctx: &mut Ctx) -> io::Result<Hash> {
    let mut from = File::open(src)?;
    let mut to = OpenOptions::new().write(true).create_new(true).open(dst)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    ctx.advance(src, 0);
    loop {
        ctx.check()?;
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        h.update(&buf[..n]);
        to.write_all(&buf[..n])?;
        ctx.advance(src, n as u64);
    }
    Ok(Hash::from_blake3(h.finalize()))
}

/// Copy a folder tree to the new path `dst`, every file confirmed. Symbolic
/// links are copied as links; sockets and the like are left out.
fn copy_dir(src: &Path, dst: &Path, ctx: &mut Ctx) -> io::Result<u64> {
    let md = fs::symlink_metadata(src)?;
    fs::create_dir(dst)?;
    let mut bytes = 0;
    for e in fs::read_dir(src)? {
        let e = e?;
        let (s, d, ft) = (e.path(), dst.join(e.file_name()), e.file_type()?);
        if rebuildable(&s) {
            continue;
        }
        if ft.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&s)?, &d)?;
        } else if ft.is_dir() {
            bytes += copy_dir(&s, &d, ctx)?;
        } else if ft.is_file() {
            bytes += copy_file(&s, &d, ctx)?;
        }
    }
    // Last, so that writing the contents does not change the date again.
    copy_meta(src, dst, &md, Kind::Dir)?;
    Ok(bytes)
}

fn copy_meta(src: &Path, dst: &Path, md: &fs::Metadata, kind: Kind) -> io::Result<()> {
    meta::write_xattrs(dst, &meta::read_xattrs(src), false)?;
    meta::set_mode(dst, kind, md.mode())?;
    meta::set_mtime(dst, kind, md.mtime() * 1_000_000_000 + md.mtime_nsec())
}
