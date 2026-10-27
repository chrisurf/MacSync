//! Carrying out a reconcile plan (see `docs/reconcile.md`): files and
//! folders of ALT copied or moved into NEU, NEU files replaced by their ALT
//! version, or both versions kept side by side.
//!
//! Nothing is ever overwritten or deleted for good: copies are written to a
//! temporary file next to the target, confirmed by BLAKE3 and renamed without
//! replacing; replaced NEU files and moved ALT sources go to the macOS Trash.
//! Every action first checks that its files are still as at the comparison.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use macsync_core::{path as rpath, Kind};
use macsync_fs::{meta, ops, time};
use serde::{Deserialize, Serialize};

use crate::walk;

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
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    #[default]
    File,
    /// A whole folder (`copy` and `move` only).
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
}

impl From<io::Error> for Stop {
    fn from(e: io::Error) -> Stop {
        Stop::Fail(e.to_string())
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
    let roots = [walk::canonical(old)?, walk::canonical(new)?];
    walk::check_roots(&roots)?;
    let mut sum = Summary { summary: true, ..Default::default() };
    for (index, a) in plan.actions.iter().enumerate() {
        let to = a.to.clone().unwrap_or_else(|| a.path.clone());
        let mut o = Outcome {
            index,
            path: a.path.clone(),
            to: to.clone(),
            result: Done::Done,
            error: None,
            trashed: None,
            bytes: 0,
        };
        match run(&roots, a, &to, trash) {
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
        }
        on(&o);
    }
    Ok(sum)
}

fn run(roots: &[PathBuf; 2], a: &Action, to: &str, trash: &Trash) -> Result<Ran, Stop> {
    for p in [a.path.as_str(), to] {
        rpath::validate(p).map_err(Stop::Fail)?;
        if p.is_empty() {
            return Err(Stop::Fail("empty path".into()));
        }
    }
    if a.kind == ItemKind::Dir && !matches!(a.op, Op::Copy | Op::Move) {
        return Err(Stop::Fail("folders can only be copied or moved".into()));
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
            let staged = if md.is_dir() { copy_dir(&src, &tmp) } else { copy_file(&src, &tmp) };
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
            ran.bytes = copy_file(&src, &tmp).inspect_err(|_| drop(fs::remove_file(&tmp)))?;
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
            ran.bytes = copy_file(&src, &tmp).inspect_err(|_| drop(fs::remove_file(&tmp)))?;
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
    }
    Ok(ran)
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
fn copy_file(src: &Path, dst: &Path) -> io::Result<u64> {
    let md = fs::symlink_metadata(src)?;
    let want = ops::hash_file(src)?;
    ops::clone_or_copy(src, dst)?;
    meta::unlock_for_write(dst);
    copy_meta(src, dst, &md, Kind::File)?;
    if ops::hash_file(dst)? != want {
        return Err(io::Error::other(format!("the copy of {} does not match the original", src.display())));
    }
    File::open(dst)?.sync_all()?;
    Ok(md.len())
}

/// Copy a folder tree to the new path `dst`, every file confirmed. Symbolic
/// links are copied as links; sockets and the like are left out.
fn copy_dir(src: &Path, dst: &Path) -> io::Result<u64> {
    let md = fs::symlink_metadata(src)?;
    fs::create_dir(dst)?;
    let mut bytes = 0;
    for e in fs::read_dir(src)? {
        let e = e?;
        let (s, d, ft) = (e.path(), dst.join(e.file_name()), e.file_type()?);
        if ft.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&s)?, &d)?;
        } else if ft.is_dir() {
            bytes += copy_dir(&s, &d)?;
        } else if ft.is_file() {
            bytes += copy_file(&s, &d)?;
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
