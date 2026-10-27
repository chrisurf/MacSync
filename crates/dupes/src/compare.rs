//! Comparing an older copy of a folder (ALT) with its newer copy (NEU): what
//! of ALT can go without losing anything?
//!
//! Files are matched by their relative path. Every file of ALT gets a state
//! (identical, newer in NEU, moved, newer in ALT, only in ALT, …); folders sum
//! up the states below them. A folder of ALT can go when nothing below it
//! would be lost. The order of the two folders is the user's assumption; the
//! dates of the changed files check it and a contradiction is reported, but
//! the roles stay as given.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use macsync_core::path as rpath;
use serde::Serialize;

use crate::walk::Tree;
use crate::{hash, walk, Content, Ctx, Options, Skipped, SAME_TIME_MS};

#[derive(Debug, Serialize)]
pub struct Comparison {
    pub old: PathBuf,
    pub new: PathBuf,
    pub old_files: u64,
    pub old_bytes: u64,
    pub new_files: u64,
    pub new_bytes: u64,
    /// Files whose content was read to compare it.
    pub hashed_files: u64,
    /// iCloud placeholders (not downloaded, compared by size and date only).
    pub dataless: u64,
    pub skipped: Vec<Skipped>,
    pub elapsed_ms: u64,
    pub order: Order,
    /// Bytes of ALT in folders that can go without losing anything.
    pub deletable_bytes: u64,
    /// Depth first, the root (`path` "") first; `children` index into it.
    pub folders: Vec<Folder>,
    /// Every file that is not identical: changed, moved, only on one side
    /// (with [`Options::files`] the identical ones as well).
    pub entries: Vec<Entry>,
}

/// Does NEU really look newer? Judged by the files that differ.
#[derive(Debug, Serialize)]
pub struct Order {
    pub status: OrderStatus,
    /// Changed files that are newer in NEU / in ALT.
    pub newer_in_new: u64,
    pub newer_in_old: u64,
    /// Latest modification time of a file on each side.
    pub old_last_ms: i64,
    pub new_last_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    /// Most changed files are newer in NEU.
    Confirmed,
    /// Most changed files are newer in ALT.
    Contradicted,
    /// No changed files, or as many newer in ALT as in NEU.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Same content in NEU (or same size and date where unreadable).
    Same,
    /// Different, NEU is newer: the ALT version is outdated.
    Updated,
    /// Not at this path in NEU, but the same content elsewhere.
    Moved,
    /// Different, ALT is newer: deleting ALT loses this change.
    OldNewer,
    /// Different content, same date.
    Unclear,
    /// Not in NEU at all.
    OnlyOld,
    /// New in NEU (information only).
    OnlyNew,
    /// Not readable here, or its place in NEU is not readable.
    Unreadable,
}

impl State {
    /// Deleting the ALT copy could lose something.
    pub fn risky(self) -> bool {
        matches!(self, State::OldNewer | State::Unclear | State::OnlyOld | State::Unreadable)
    }
}

#[derive(Debug, Serialize)]
pub struct Entry {
    /// Relative path, spelled as in ALT (as in NEU for `only_new`).
    pub path: String,
    pub state: State,
    pub size: u64,
    /// Size of the NEU file, where there is one (the moved-to file for
    /// `moved`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_new: Option<u64>,
    pub mtime_old: Option<i64>,
    pub mtime_new: Option<i64>,
    /// `moved`: where the content is in NEU.
    pub moved_to: Option<String>,
    /// `only_old`: where it belongs in NEU because most of its folder was
    /// moved there (absent: the same path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested: Option<String>,
    /// Spelling in NEU, if it differs from `path` (case, normalisation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_new: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Counts {
    pub same: u64,
    pub updated: u64,
    pub moved: u64,
    pub old_newer: u64,
    pub unclear: u64,
    pub only_old: u64,
    pub only_new: u64,
    pub unreadable: u64,
}

impl Counts {
    fn add(&mut self, s: State) {
        *match s {
            State::Same => &mut self.same,
            State::Updated => &mut self.updated,
            State::Moved => &mut self.moved,
            State::OldNewer => &mut self.old_newer,
            State::Unclear => &mut self.unclear,
            State::OnlyOld => &mut self.only_old,
            State::OnlyNew => &mut self.only_new,
            State::Unreadable => &mut self.unreadable,
        } += 1;
    }

    fn merge(&mut self, o: &Counts) {
        self.same += o.same;
        self.updated += o.updated;
        self.moved += o.moved;
        self.old_newer += o.old_newer;
        self.unclear += o.unclear;
        self.only_old += o.only_old;
        self.only_new += o.only_new;
        self.unreadable += o.unreadable;
    }

    pub fn risky(&self) -> u64 {
        self.old_newer + self.unclear + self.only_old + self.unreadable
    }

    /// Files of ALT.
    pub fn old_files(&self) -> u64 {
        self.same + self.updated + self.moved + self.old_newer + self.unclear + self.only_old + self.unreadable
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderStatus {
    /// Same files, same contents.
    Identical,
    /// Everything is in NEU (identical, newer or moved): ALT's copy can go.
    Deletable,
    /// Something would be lost: newer in ALT, only in ALT, unclear.
    Check,
    /// The whole folder exists in ALT only.
    OnlyOld,
    /// The whole folder exists in NEU only.
    OnlyNew,
    /// Not fully readable: no statement possible.
    Unreadable,
}

impl FolderStatus {
    /// Deleting the ALT copy of the folder loses nothing.
    pub fn deletable(self) -> bool {
        matches!(self, FolderStatus::Identical | FolderStatus::Deletable)
    }
}

#[derive(Debug, Serialize)]
pub struct Folder {
    /// Relative path (`""` for the root), spelled as in ALT if it is there.
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub status: FolderStatus,
    pub in_old: bool,
    pub in_new: bool,
    /// Files directly in this folder.
    pub own: Counts,
    /// Files in this folder and below.
    pub total: Counts,
    pub old_bytes: u64,
    pub new_bytes: u64,
    pub children: Vec<usize>,
    /// Indices into `Comparison::entries` of the files directly in here.
    pub entries: Vec<usize>,
}

/// Compare `old` (ALT) with `new` (NEU).
pub fn compare(old: &Path, new: &Path, opts: &Options) -> io::Result<Comparison> {
    let start = Instant::now();
    let abs = vec![walk::canonical(old)?, walk::canonical(new)?];
    walk::check_roots(&abs)?;
    let tree = walk::walk(&abs, &crate::exclude_set(opts)?, opts.skip_caches, opts.progress)?;
    let t = &tree;

    // Relative key path → node, per side.
    let keys: Vec<String> = (0..t.nodes.len()).map(|i| t.key_below(t.roots[t.nodes[i].root], i)).collect();
    let mut by: [HashMap<&str, usize>; 2] = [HashMap::new(), HashMap::new()];
    for (i, k) in keys.iter().enumerate() {
        by[t.nodes[i].root].insert(k.as_str(), i);
    }
    let files = |side: usize| t.files().filter(move |&i| t.nodes[i].root == side);
    let partner = |i: usize| by[1].get(keys[i].as_str()).copied().filter(|&j| !t.nodes[j].dir);

    // Read contents only where they can decide something: same path and same
    // size, or a file missing on one side with a same-sized file missing on
    // the other (moved).
    let readable = |i: usize| t.nodes[i].size > 0 && !t.nodes[i].dataless;
    let mut todo: HashSet<usize> = HashSet::new();
    let mut lone_old = Vec::new();
    for i in files(0) {
        match partner(i) {
            Some(j) if t.nodes[i].size == t.nodes[j].size && readable(i) && readable(j) => {
                todo.extend([i, j]);
            }
            Some(_) => {}
            None => lone_old.push(i),
        }
    }
    let lone_new: Vec<usize> =
        files(1).filter(|&j| by[0].get(keys[j].as_str()).is_none_or(|&i| t.nodes[i].dir)).collect();
    let sizes: HashSet<u64> = lone_new.iter().filter(|&&j| readable(j)).map(|&j| t.nodes[j].size).collect();
    let mut moved_sizes = HashSet::new();
    for &i in &lone_old {
        if readable(i) && sizes.contains(&t.nodes[i].size) {
            todo.insert(i);
            moved_sizes.insert(t.nodes[i].size);
        }
    }
    todo.extend(lone_new.iter().filter(|&&j| readable(j) && moved_sizes.contains(&t.nodes[j].size)));
    let mut todo: Vec<usize> = todo.into_iter().collect();
    todo.sort_unstable();
    let (hashes, _) = hash::hash_all(t, &todo, opts.progress);
    let labels = vec!["ALT".to_string(), "NEU".to_string()];
    let ctx = Ctx { t, h: &hashes, labels: &labels };
    let mut new_by_hash: HashMap<[u8; 32], usize> = HashMap::new();
    for &j in &lone_new {
        if let Some(h) = hashes.get(&j) {
            new_by_hash.entry(*h).or_insert(j);
        }
    }

    // Unreadable places, as key paths of their side.
    let mut unreadable: [Vec<String>; 2] = [Vec::new(), Vec::new()];
    for s in &t.skipped {
        for (side, root) in abs.iter().enumerate() {
            if let Ok(rel) = s.path.strip_prefix(root) {
                unreadable[side].push(key_path(rel));
            }
        }
    }
    let below_unreadable = |side: usize, k: &str| {
        unreadable[side].iter().any(|u| u.is_empty() || k == u || k.starts_with(&format!("{u}/")))
    };

    let mut entries = Vec::new();
    let (mut newer_in_new, mut newer_in_old) = (0, 0);
    let mut moved_targets = HashSet::new();
    for i in files(0) {
        let n = &t.nodes[i];
        let mut e = Entry {
            path: n.rel.clone(),
            state: State::Same,
            size: n.size,
            size_new: None,
            mtime_old: Some(n.mtime_ms),
            mtime_new: None,
            moved_to: None,
            suggested: None,
            path_new: None,
        };
        match partner(i) {
            Some(j) => {
                let dt = t.nodes[j].mtime_ms - n.mtime_ms;
                e.mtime_new = Some(t.nodes[j].mtime_ms);
                e.size_new = Some(t.nodes[j].size);
                e.path_new = Some(t.nodes[j].rel.clone()).filter(|p| *p != n.rel);
                let same = ctx.content(i, j) == Content::Equal
                    || (ctx.content(i, j) == Content::Unknown && dt.abs() <= SAME_TIME_MS);
                e.state = if same {
                    State::Same
                } else if dt > SAME_TIME_MS {
                    newer_in_new += 1;
                    State::Updated
                } else if dt < -SAME_TIME_MS {
                    newer_in_old += 1;
                    State::OldNewer
                } else {
                    State::Unclear
                };
            }
            None => match hashes.get(&i).and_then(|h| new_by_hash.get(h)) {
                Some(&j) => {
                    e.state = State::Moved;
                    e.mtime_new = Some(t.nodes[j].mtime_ms);
                    e.size_new = Some(t.nodes[j].size);
                    e.moved_to = Some(t.nodes[j].rel.clone());
                    moved_targets.insert(j);
                }
                None if below_unreadable(1, &keys[i]) => e.state = State::Unreadable,
                None => e.state = State::OnlyOld,
            },
        }
        entries.push((keys[i].clone(), e));
    }
    for &j in &lone_new {
        // The target of a move is explained by the move.
        if moved_targets.contains(&j) {
            continue;
        }
        let n = &t.nodes[j];
        let e = Entry {
            path: n.rel.clone(),
            state: State::OnlyNew,
            size: n.size,
            size_new: Some(n.size),
            mtime_old: None,
            mtime_new: Some(n.mtime_ms),
            moved_to: None,
            suggested: None,
            path_new: None,
        };
        entries.push((keys[j].clone(), e));
    }
    suggest(&mut entries, &by[1]);
    // An unreadable spot of ALT hides files: its folder cannot be judged.
    let mut hidden: Vec<String> = unreadable[0].clone();
    hidden.retain(|k| !k.is_empty());

    let old_last_ms = files(0).map(|i| t.nodes[i].mtime_ms).max().unwrap_or(0);
    let new_last_ms = files(1).map(|j| t.nodes[j].mtime_ms).max().unwrap_or(0);
    let status = match newer_in_new.cmp(&newer_in_old) {
        std::cmp::Ordering::Greater => OrderStatus::Confirmed,
        std::cmp::Ordering::Less => OrderStatus::Contradicted,
        std::cmp::Ordering::Equal => OrderStatus::Unknown,
    };

    let (folders, entries) = build_folders(t, &keys, &mut entries, &hidden, opts.files);
    let deletable_bytes = deletable(&folders, 0);
    let side_sum = |side: usize| {
        let (mut n, mut b) = (0, 0);
        for i in files(side) {
            n += 1;
            b += t.nodes[i].size;
        }
        (n, b)
    };
    let ((old_files, old_bytes), (new_files, new_bytes)) = (side_sum(0), side_sum(1));
    Ok(Comparison {
        old: abs[0].clone(),
        new: abs[1].clone(),
        old_files,
        old_bytes,
        new_files,
        new_bytes,
        hashed_files: todo.len() as u64,
        dataless: t.files().filter(|&i| t.nodes[i].dataless).count() as u64,
        skipped: tree.skipped,
        elapsed_ms: start.elapsed().as_millis() as u64,
        order: Order { status, newer_in_new, newer_in_old, old_last_ms, new_last_ms },
        deletable_bytes,
        folders,
        entries,
    })
}

struct Build {
    path: String,
    name: String,
    in_old: bool,
    in_new: bool,
    own: Counts,
    old_bytes: u64,
    new_bytes: u64,
    entries: Vec<usize>,
}

/// Folders of both sides, keyed by relative key path, with the file states
/// summed up from the bottom.
fn build_folders(
    t: &Tree,
    keys: &[String],
    entries: &mut Vec<(String, Entry)>,
    hidden: &[String],
    keep_same: bool,
) -> (Vec<Folder>, Vec<Entry>) {
    let mut map: BTreeMap<String, Build> = BTreeMap::new();
    let ensure = |map: &mut BTreeMap<String, Build>, key: &str, path: &str| {
        map.entry(key.to_string()).or_insert_with(|| Build {
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or("").to_string(),
            in_old: false,
            in_new: false,
            own: Counts::default(),
            old_bytes: 0,
            new_bytes: 0,
            entries: Vec::new(),
        });
    };
    for (i, n) in t.nodes.iter().enumerate() {
        if n.dir {
            ensure(&mut map, &keys[i], &n.rel);
            let b = map.get_mut(&keys[i]).unwrap();
            if n.root == 0 {
                b.in_old = true;
                // The spelling of ALT wins.
                b.path = n.rel.clone();
                b.name = b.path.rsplit('/').next().unwrap_or("").to_string();
            } else {
                b.in_new = true;
            }
        }
    }
    for h in hidden {
        // An unreadable folder counts itself, an unreadable file its folder.
        let k = if map.contains_key(h) { h.clone() } else { parent(h) };
        if let Some(b) = map.get_mut(&k) {
            b.own.unreadable += 1;
        }
    }

    // Identical files are only counted, not listed (unless asked for).
    let mut kept = Vec::new();
    for (key, e) in entries.drain(..) {
        let b = map.get_mut(&parent(&key)).expect("the folder of a file was scanned");
        b.own.add(e.state);
        if e.state == State::OnlyNew {
            b.new_bytes += e.size;
        } else {
            b.old_bytes += e.size;
        }
        if keep_same || e.state != State::Same {
            b.entries.push(kept.len());
            kept.push(e);
        }
    }

    // Depth first: children sorted by name.
    let mut kids: HashMap<String, Vec<String>> = HashMap::new();
    for k in map.keys().filter(|k| !k.is_empty()) {
        kids.entry(parent(k)).or_default().push(k.clone());
    }
    let mut out: Vec<Folder> = Vec::new();
    fn visit(
        k: &str,
        depth: usize,
        map: &BTreeMap<String, Build>,
        kids: &HashMap<String, Vec<String>>,
        out: &mut Vec<Folder>,
    ) -> usize {
        let b = &map[k];
        let me = out.len();
        out.push(Folder {
            path: b.path.clone(),
            name: b.name.clone(),
            depth,
            status: FolderStatus::Identical,
            in_old: b.in_old,
            in_new: b.in_new,
            own: b.own,
            total: b.own,
            old_bytes: b.old_bytes,
            new_bytes: b.new_bytes,
            children: Vec::new(),
            entries: b.entries.clone(),
        });
        let mut ch: Vec<&String> = kids.get(k).map(|v| v.iter().collect()).unwrap_or_default();
        ch.sort_by_key(|c| map[*c].name.to_lowercase());
        for c in ch {
            let ci = visit(c, depth + 1, map, kids, out);
            let (total, ob, nb) = (out[ci].total, out[ci].old_bytes, out[ci].new_bytes);
            let f = &mut out[me];
            f.children.push(ci);
            f.total.merge(&total);
            f.old_bytes += ob;
            f.new_bytes += nb;
        }
        out[me].status = status(&out[me]);
        me
    }
    visit("", 0, &map, &kids, &mut out);
    (out, kept)
}

/// Target paths in NEU for `only_old` files: if most files below a folder of
/// ALT were moved to one other folder of NEU (keeping the layout below it),
/// the missing ones belong there as well. The nearest such folder wins.
fn suggest(entries: &mut [(String, Entry)], new_keys: &HashMap<&str, usize>) {
    let ancestors = |k: &str| {
        let mut out = vec![String::new()];
        let mut acc = String::new();
        let comps: Vec<&str> = k.split('/').collect();
        for c in &comps[..comps.len() - 1] {
            acc = rpath::join(&acc, c);
            out.push(acc.clone());
        }
        out
    };
    // Files of ALT below each folder, and where moved ones went.
    let mut files: HashMap<String, u64> = HashMap::new();
    let mut votes: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for (k, e) in entries.iter() {
        if e.state == State::OnlyNew {
            continue;
        }
        for a in ancestors(k) {
            *files.entry(a).or_default() += 1;
        }
        let Some(to) = e.moved_to.as_deref().filter(|_| e.state == State::Moved) else { continue };
        let from: Vec<&str> = k.split('/').collect();
        let to: Vec<&str> = to.split('/').collect();
        let (from, to) = (&from[..from.len() - 1], &to[..to.len() - 1]);
        // Shared trailing folders: each level up maps to the target's level.
        let shared = from.iter().rev().zip(to.iter().rev()).take_while(|(a, b)| **a == rpath::case_key(b)).count();
        for up in 0..=shared {
            let (a, b) = (from[..from.len() - up].join("/"), to[..to.len() - up].join("/"));
            if a != key_path(Path::new(&b)) {
                *votes.entry(a).or_default().entry(b).or_default() += 1;
            }
        }
    }
    let target: HashMap<&str, &str> = votes
        .iter()
        .filter_map(|(a, v)| {
            let (b, n) = v.iter().max_by_key(|(b, n)| (**n, std::cmp::Reverse(b.as_str())))?;
            (n * 2 > files.get(a).copied().unwrap_or(0)).then_some((a.as_str(), b.as_str()))
        })
        .collect();
    let mut found = Vec::new();
    for (i, (k, e)) in entries.iter().enumerate() {
        if e.state != State::OnlyOld {
            continue;
        }
        let Some(a) = ancestors(k).into_iter().rev().find(|a| target.contains_key(a.as_str())) else { continue };
        let depth = if a.is_empty() { 0 } else { a.split('/').count() };
        let rest = e.path.split('/').skip(depth).collect::<Vec<_>>().join("/");
        let s = rpath::join(target[a.as_str()], &rest);
        if !new_keys.contains_key(key_path(Path::new(&s)).as_str()) {
            found.push((i, s));
        }
    }
    for (i, s) in found {
        entries[i].1.suggested = Some(s);
    }
}

fn status(f: &Folder) -> FolderStatus {
    let c = &f.total;
    if !f.in_old {
        FolderStatus::OnlyNew
    } else if c.unreadable > 0 {
        FolderStatus::Unreadable
    } else if c.risky() > 0 {
        if !f.in_new && c.only_old == c.old_files() {
            FolderStatus::OnlyOld
        } else {
            FolderStatus::Check
        }
    } else if f.in_new && c.updated + c.moved + c.only_new == 0 {
        FolderStatus::Identical
    } else {
        FolderStatus::Deletable
    }
}

/// Bytes of ALT in the outermost folders that can go.
fn deletable(folders: &[Folder], i: usize) -> u64 {
    let f = &folders[i];
    if f.status.deletable() {
        f.old_bytes
    } else {
        f.children.iter().map(|&c| deletable(folders, c)).sum()
    }
}

fn parent(key: &str) -> String {
    key.rsplit_once('/').map_or(String::new(), |(p, _)| p.to_string())
}

fn key_path(rel: &Path) -> String {
    rel.components().map(|c| rpath::case_key(&c.as_os_str().to_string_lossy())).collect::<Vec<_>>().join("/")
}
