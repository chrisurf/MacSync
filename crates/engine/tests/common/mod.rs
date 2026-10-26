#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use macsync_engine::{Engine, EngineConfig, Mode, PullOptions, PullReport};

pub struct Pair {
    pub a: Engine,
    pub b: Engine,
    pub dir: tempfile::TempDir,
}

pub fn engine(base: &Path, name: &str, mode: Mode) -> Engine {
    let mut cfg = EngineConfig::new(base.join(format!("{name}-root")), base.join(format!("{name}-state")), name);
    cfg.mode = mode;
    Engine::open(cfg).unwrap()
}

impl Pair {
    pub fn new() -> Pair {
        Self::with_modes(Mode::TwoWay, Mode::TwoWay)
    }

    pub fn with_modes(ma: Mode, mb: Mode) -> Pair {
        let dir = tempfile::tempdir().unwrap();
        let a = engine(dir.path(), "devA", ma);
        let b = engine(dir.path(), "devB", mb);
        Pair { a, b, dir }
    }

    pub fn ra(&self) -> PathBuf {
        self.a.root().to_path_buf()
    }

    pub fn rb(&self) -> PathBuf {
        self.b.root().to_path_buf()
    }

    /// Scan both sides and pull in both directions until nothing changes.
    pub fn sync(&self) -> Vec<PullReport> {
        let mut all = Vec::new();
        for _ in 0..4 {
            self.a.scan_all(false).unwrap();
            self.b.scan_all(false).unwrap();
            let r1 = self.b.sync_from(&self.a, None, PullOptions::default()).unwrap();
            let r2 = self.a.sync_from(&self.b, None, PullOptions::default()).unwrap();
            for r in r1.iter().chain(r2.iter()) {
                if let macsync_engine::Outcome::Failed(e) = &r.outcome {
                    panic!("pull of {} failed: {e}", r.unit);
                }
            }
            let quiet = r1.iter().chain(r2.iter()).all(|r| r.outcome == macsync_engine::Outcome::InSync);
            all.extend(r1);
            all.extend(r2);
            if quiet {
                break;
            }
        }
        all
    }

    pub fn roots_equal(&self) -> bool {
        let a: BTreeMap<_, _> = self.a.local_roots().unwrap().into_iter().map(|r| (r.name, r.root)).collect();
        let b: BTreeMap<_, _> = self.b.local_roots().unwrap().into_iter().map(|r| (r.name, r.root)).collect();
        a == b
    }

    pub fn assert_identical(&self) {
        assert!(self.roots_equal(), "roots differ");
        assert_eq!(snapshot(&self.ra()), snapshot(&self.rb()), "trees differ");
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Node {
    File { mode: u32, content: Vec<u8> },
    Dir { mode: u32 },
    Link(PathBuf),
}

/// Recursive snapshot of a directory (excluding `.macsync`, process-local
/// Git files and the raw index, which is compared semantically).
pub fn snapshot(root: &Path) -> BTreeMap<String, Node> {
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Node>) {
    let mut rd: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
    rd.sort_by_key(|e| e.file_name());
    for e in rd {
        let p = e.path();
        let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        if rel == ".macsync" || rel.ends_with(".lock") {
            continue;
        }
        let md = fs::symlink_metadata(&p).unwrap();
        let mode = md.permissions().mode() & 0o7777;
        if md.file_type().is_symlink() {
            out.insert(rel, Node::Link(fs::read_link(&p).unwrap()));
        } else if md.is_dir() {
            out.insert(rel, Node::Dir { mode });
            walk(root, &p, out);
        } else if rel.ends_with(".git/index") {
            let data = fs::read(&p).unwrap();
            let sem = macsync_gitaware::index::semantic_hash(&data, 20).map(|h| h.0.to_vec()).unwrap_or(data);
            out.insert(rel, Node::File { mode, content: sem });
        } else {
            out.insert(rel, Node::File { mode, content: fs::read(&p).unwrap() });
        }
    }
}

pub fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

pub fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap()
}

pub fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=Tester", "-c", "user.email=t@example.com", "-c", "init.defaultBranch=main"])
        .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} in {}: {}", dir.display(), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git").current_dir(dir).args(args).output().is_ok_and(|o| o.status.success())
}

/// `git fsck --full` must succeed and report nothing broken.
pub fn assert_fsck(dir: &Path) {
    let out = Command::new("git").current_dir(dir).args(["fsck", "--full", "--no-dangling"]).output().unwrap();
    assert!(
        out.status.success(),
        "fsck failed in {}: {}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// All refs, stashes and HEAD of a repository (to prove MacSync never
/// creates history).
pub fn refs(dir: &Path) -> String {
    let mut s = git(dir, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    s.push_str(&git(dir, &["rev-parse", "HEAD"]));
    s
}

pub fn init_repo(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    fs::write(dir.join("README.md"), "# demo\n").unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "initial"]);
}
